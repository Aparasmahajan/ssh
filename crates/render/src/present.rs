//! Damage-only presentation on Vulkan (Linux).
//!
//! wgpu's own surface repaints and presents the whole window every frame: it
//! treats each acquired image as uninitialised, so it clears it, and it has no
//! way to say which part changed. For a keystroke that is one changed cell
//! costing a full-surface clear, draw, copy and composite. Measured on Iris Xe
//! with the GPU clock pinned, latency followed area: 3.2 ms p50 at 1006×894
//! against 2.1 ms at 366×494 (COMPLETED.md, Phase 20).
//!
//! So on Vulkan this module owns the swapchain, underneath wgpu:
//!
//! 1. The renderer draws into a persistent **canvas** texture, only the band of
//!    rows that changed ([`crate::Renderer::draw_damage`]). Plain wgpu.
//! 2. Each swapchain image remembers the frame it was last brought up to date.
//!    Presenting copies into it, from the canvas, the union of damage since
//!    then. Raw Vulkan, one small command buffer.
//! 3. The present names this frame's damage with `VK_KHR_incremental_present`,
//!    so the X server copies and the compositor recomposites only that.
//!
//! The canvas is a wgpu texture whose layout wgpu tracks as `COLOR_TARGET`
//! (`COLOR_ATTACHMENT_OPTIMAL`) after every render pass. The copy moves it to
//! `TRANSFER_SRC_OPTIMAL` and back in the same command buffer, so wgpu's view
//! of it is true again before wgpu next records anything. Nothing else of
//! wgpu's is touched.
//!
//! Every failure here is reported, never panicked on; the caller falls back
//! to wgpu's own surface.

use std::collections::VecDeque;

use ash::vk;
use wgpu::hal::api::Vulkan;

use crate::frame::Rect;

/// Frames of damage remembered for bringing older images up to date. An image
/// older than this gets a full copy, which is only ever a cost, never wrong.
const HISTORY: usize = 16;

/// Frames that may be in flight on the CPU side (command buffer, acquire
/// semaphore, fence).
const SLOTS: usize = 2;

/// Open a device on `adapter` with `VK_KHR_incremental_present` enabled when
/// the driver has it. `None` if the adapter is not Vulkan or opening fails;
/// the caller then opens a device the ordinary way. The flag says whether the
/// extension is on.
pub fn open_device(adapter: &wgpu::Adapter) -> Option<(wgpu::Device, wgpu::Queue, bool)> {
    let desc = wgpu::DeviceDescriptor::default();
    // SAFETY: the hal adapter is only used to open a device on itself, which
    // is then handed straight back to wgpu (`create_device_from_hal`).
    let open = unsafe {
        let hal = adapter.as_hal::<Vulkan>()?;
        let shared = hal.shared_instance();
        let exts = shared.raw_instance().enumerate_device_extension_properties(hal.raw_physical_device()).ok()?;
        let want = ash::khr::incremental_present::NAME;
        let incremental = exts.iter().any(|e| e.extension_name_as_c_str() == Ok(want));
        let open = hal
            .open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(Box::new(move |args: wgpu::hal::vulkan::CreateDeviceCallbackArgs| {
                    if incremental {
                        args.extensions.push(want);
                    }
                })),
            )
            .ok()?;
        (open, incremental)
    };
    // SAFETY: `open` was created from this adapter with exactly `desc`'s
    // features and limits.
    let (device, queue) = unsafe { adapter.create_device_from_hal(open.0, &desc) }.ok()?;
    Some((device, queue, open.1))
}

/// What a frame must draw before [`Presenter::present`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// The canvas holds the previous frame: draw only the damage.
    Damage,
    /// The canvas is new (first frame, resize, swapchain rebuilt): draw it all.
    Full,
}

struct Slot {
    cmd: vk::CommandBuffer,
    /// Signalled when this slot's copy has executed.
    fence: vk::Fence,
    acquired: vk::Semaphore,
}

pub struct Presenter {
    device: wgpu::Device,
    raw: ash::Device,
    queue: vk::Queue,
    swapchain_fn: ash::khr::swapchain::Device,
    surface_fn: ash::khr::surface::Instance,
    phd: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
    format: wgpu::TextureFormat,
    vk_format: vk::Format,
    incremental: bool,
    pool: vk::CommandPool,
    slots: Vec<Slot>,

    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    /// Frame each image was last brought up to date in; `None` if never
    /// (its layout is then `UNDEFINED`, else `PRESENT_SRC_KHR`).
    written: Vec<Option<u64>>,
    /// Signalled by the copy into image `i`, waited on by its present. One per
    /// image: an image cannot be acquired again before its present consumed it.
    done: Vec<vk::Semaphore>,
    canvas: Option<(wgpu::Texture, wgpu::TextureView, vk::Image)>,
    size: (u32, u32),
    frame: u64,
    history: VecDeque<(u64, Rect)>,
    /// The swapchain must be rebuilt before the next frame.
    stale: bool,
    pub present_mode: vk::PresentModeKHR,
}

fn vk_format(f: wgpu::TextureFormat) -> Option<vk::Format> {
    match f {
        wgpu::TextureFormat::Bgra8Unorm => Some(vk::Format::B8G8R8A8_UNORM),
        wgpu::TextureFormat::Rgba8Unorm => Some(vk::Format::R8G8B8A8_UNORM),
        _ => None,
    }
}

fn err(what: &str) -> impl Fn(vk::Result) -> String + '_ {
    move |e| format!("{what}: {e}")
}

const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

impl Presenter {
    /// A presenter for `surface`, drawing in `format` (the renderer's). The
    /// wgpu surface must never be configured while this exists: it owns the
    /// surface's swapchain. Drop this before the surface.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: &wgpu::Surface<'_>,
        format: wgpu::TextureFormat,
        incremental: bool,
    ) -> Result<Presenter, String> {
        let vk_format = vk_format(format).ok_or_else(|| format!("no Vulkan format for {format:?}"))?;
        // SAFETY: handles are copied out of the guards and used only while
        // `device` (kept below) and the caller's surface are alive; the
        // swapchain and pool created here are destroyed in Drop, after a
        // device-idle wait.
        unsafe {
            let hal_dev = device.as_hal::<Vulkan>().ok_or("not a Vulkan device")?;
            let raw = hal_dev.raw_device().clone();
            let phd = hal_dev.raw_physical_device();
            let family = hal_dev.queue_family_index();
            let shared = hal_dev.shared_instance();
            let swapchain_fn = ash::khr::swapchain::Device::new(shared.raw_instance(), &raw);
            let surface_fn = ash::khr::surface::Instance::new(shared.entry(), shared.raw_instance());
            drop(hal_dev);
            let vkq = queue.as_hal::<Vulkan>().ok_or("not a Vulkan queue")?.as_raw();
            let vk_surface = surface
                .as_hal::<Vulkan>()
                .and_then(|s| s.raw_native_handle())
                .ok_or("not a Vulkan surface")?;
            let formats = surface_fn
                .get_physical_device_surface_formats(phd, vk_surface)
                .map_err(err("surface formats"))?;
            if !formats.iter().any(|f| f.format == vk_format) {
                return Err(format!("surface cannot present {vk_format:?}"));
            }

            let pool = raw
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                        .queue_family_index(family),
                    None,
                )
                .map_err(err("command pool"))?;
            let mut p = Presenter {
                device: device.clone(),
                raw,
                queue: vkq,
                swapchain_fn,
                surface_fn,
                phd,
                surface: vk_surface,
                format,
                vk_format,
                incremental,
                pool,
                slots: Vec::new(),
                swapchain: vk::SwapchainKHR::null(),
                images: Vec::new(),
                written: Vec::new(),
                done: Vec::new(),
                canvas: None,
                size: (0, 0),
                frame: 0,
                history: VecDeque::new(),
                stale: true,
                present_mode: vk::PresentModeKHR::FIFO,
            };
            // From here on, Drop cleans up whatever exists.
            let cmds = p
                .raw
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(SLOTS as u32),
                )
                .map_err(err("command buffers"))?;
            for cmd in cmds {
                let fence = p
                    .raw
                    .create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)
                    .map_err(err("fence"))?;
                let acquired = p.raw.create_semaphore(&Default::default(), None).map_err(err("semaphore"))?;
                p.slots.push(Slot { cmd, fence, acquired });
            }
            Ok(p)
        }
    }

    pub fn incremental(&self) -> bool {
        self.incremental
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The texture to draw this frame into.
    pub fn canvas(&self) -> Option<&wgpu::TextureView> {
        self.canvas.as_ref().map(|c| &c.1)
    }

    /// Get ready to draw a frame at `size` (physical pixels). Rebuilds the
    /// swapchain and canvas if the size changed or the last present said so,
    /// in which case the whole canvas must be drawn. The size actually used
    /// can differ from `size` while a resize is in flight: read [`size`].
    ///
    /// [`size`]: Presenter::size
    pub fn begin(&mut self, size: (u32, u32)) -> Result<Need, String> {
        if !self.stale && size == self.size && self.canvas.is_some() {
            return Ok(Need::Damage);
        }
        self.rebuild(size)?;
        Ok(Need::Full)
    }

    fn rebuild(&mut self, want: (u32, u32)) -> Result<(), String> {
        // SAFETY: every handle below belongs to `self.raw`; the old swapchain
        // and semaphores are destroyed only after the device is idle.
        let size = unsafe {
            let _ = self.raw.device_wait_idle();
            let caps = self
                .surface_fn
                .get_physical_device_surface_capabilities(self.phd, self.surface)
                .map_err(err("surface capabilities"))?;
            if !caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_DST) {
                return Err("swapchain images cannot be copied into".into());
            }
            // X11 reports the window's size; follow it rather than our guess.
            let extent = if caps.current_extent.width != u32::MAX {
                caps.current_extent
            } else {
                vk::Extent2D { width: want.0, height: want.1 }
            };
            if extent.width == 0 || extent.height == 0 {
                return Err("window has no area".into());
            }
            let modes = self
                .surface_fn
                .get_physical_device_surface_present_modes(self.phd, self.surface)
                .map_err(err("present modes"))?;
            // Immediate measured lowest (Mailbox and Fifo were 3× slower).
            // Under a compositor the window is redirected, so it does not tear.
            self.present_mode = [vk::PresentModeKHR::IMMEDIATE, vk::PresentModeKHR::MAILBOX]
                .into_iter()
                .find(|m| modes.contains(m))
                .unwrap_or(vk::PresentModeKHR::FIFO);
            let alpha = [vk::CompositeAlphaFlagsKHR::OPAQUE, vk::CompositeAlphaFlagsKHR::INHERIT]
                .into_iter()
                .find(|a| caps.supported_composite_alpha.contains(*a))
                .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);
            let mut count = caps.min_image_count.max(2);
            if caps.max_image_count > 0 {
                count = count.min(caps.max_image_count);
            }
            let old = self.swapchain;
            let info = vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(count)
                .image_format(self.vk_format)
                .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(alpha)
                .present_mode(self.present_mode)
                .clipped(true)
                .old_swapchain(old);
            let made = self.swapchain_fn.create_swapchain(&info, None);
            if old != vk::SwapchainKHR::null() {
                self.swapchain_fn.destroy_swapchain(old, None);
                self.swapchain = vk::SwapchainKHR::null();
            }
            for s in self.done.drain(..) {
                self.raw.destroy_semaphore(s, None);
            }
            self.swapchain = made.map_err(err("swapchain"))?;
            self.images = self.swapchain_fn.get_swapchain_images(self.swapchain).map_err(err("swapchain images"))?;
            self.written = vec![None; self.images.len()];
            for _ in 0..self.images.len() {
                self.done.push(self.raw.create_semaphore(&Default::default(), None).map_err(err("semaphore"))?);
            }
            (extent.width, extent.height)
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("term canvas"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        // SAFETY: the raw image lives as long as `tex`, which is kept with it.
        let image = unsafe { tex.as_hal::<Vulkan>().map(|t| t.raw_handle()) }.ok_or("canvas is not a Vulkan texture")?;
        self.canvas = Some((tex, view, image));
        self.size = size;
        self.history.clear();
        self.stale = false;
        Ok(())
    }

    /// Show the canvas. Call after the frame's drawing into [`canvas`] was
    /// submitted to the queue. `damage` is what changed this frame. On `false`
    /// the frame was not shown (the window changed under it): draw again, it
    /// will be a full frame.
    ///
    /// [`canvas`]: Presenter::canvas
    pub fn present(&mut self, damage: Rect) -> Result<bool, String> {
        let Some(canvas) = self.canvas.as_ref().map(|c| c.2) else {
            return Ok(false);
        };
        let Some(damage) = damage.clip(self.size) else { return Ok(true) };
        self.frame += 1;
        let frame = self.frame;
        self.history.push_back((frame, damage));
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        let slot = &self.slots[(frame % SLOTS as u64) as usize];
        let (cmd, fence, acquired) = (slot.cmd, slot.fence, slot.acquired);

        // SAFETY: all handles are this presenter's and alive. The slot's fence
        // guards reuse of its command buffer and acquire semaphore; `done[i]`
        // is reused only after image `i` is acquired again, which the
        // presentation engine allows only once its previous present finished
        // waiting on it.
        unsafe {
            self.raw.wait_for_fences(&[fence], true, u64::MAX).map_err(err("fence"))?;
            let index = match self.swapchain_fn.acquire_next_image(self.swapchain, u64::MAX, acquired, vk::Fence::null()) {
                Ok((i, _)) => i as usize,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.stale = true;
                    return Ok(false);
                }
                Err(e) => return Err(format!("acquire: {e}")),
            };
            self.raw.reset_fences(&[fence]).map_err(err("fence"))?;

            // What this image is missing: everything since it was last
            // written, or all of it.
            let full = Rect::full(self.size);
            let copy = match self.written[index] {
                Some(w) if self.history.front().is_some_and(|(f, _)| *f <= w + 1) => self
                    .history
                    .iter()
                    .filter(|(f, _)| *f > w)
                    .map(|(_, r)| *r)
                    .reduce(Rect::union)
                    .unwrap_or(damage),
                _ => full,
            };
            let image = self.images[index];
            let old_layout = if self.written[index].is_some() {
                vk::ImageLayout::PRESENT_SRC_KHR
            } else {
                vk::ImageLayout::UNDEFINED
            };

            self.raw.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).map_err(err("reset"))?;
            self.raw
                .begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))
                .map_err(err("begin"))?;
            let barrier = |img, old, new, src: vk::AccessFlags, dst: vk::AccessFlags| {
                vk::ImageMemoryBarrier::default()
                    .src_access_mask(src)
                    .dst_access_mask(dst)
                    .old_layout(old)
                    .new_layout(new)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(img)
                    .subresource_range(COLOR)
            };
            // The canvas was last written by wgpu's render pass (an earlier
            // submission on this queue); the image waits on the acquire
            // semaphore at the transfer stage.
            self.raw.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[
                    barrier(
                        canvas,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                        vk::AccessFlags::TRANSFER_READ,
                    ),
                    barrier(
                        image,
                        old_layout,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags::empty(),
                        vk::AccessFlags::TRANSFER_WRITE,
                    ),
                ],
            );
            let layers = vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            };
            let at = vk::Offset3D { x: copy.x as i32, y: copy.y as i32, z: 0 };
            self.raw.cmd_copy_image(
                cmd,
                canvas,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageCopy {
                    src_subresource: layers,
                    src_offset: at,
                    dst_subresource: layers,
                    dst_offset: at,
                    extent: vk::Extent3D { width: copy.w, height: copy.h, depth: 1 },
                }],
            );
            // Hand the canvas back to wgpu in the layout it believes it is in,
            // and the image to the presentation engine.
            self.raw.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[
                    barrier(
                        canvas,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::AccessFlags::empty(),
                        vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    ),
                    barrier(
                        image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::PRESENT_SRC_KHR,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::empty(),
                    ),
                ],
            );
            self.raw.end_command_buffer(cmd).map_err(err("end"))?;

            let waits = [acquired];
            let stages = [vk::PipelineStageFlags::TRANSFER];
            let cmds = [cmd];
            let signals = [self.done[index]];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&cmds)
                .signal_semaphores(&signals);
            self.raw.queue_submit(self.queue, &[submit], fence).map_err(err("submit"))?;
            self.written[index] = Some(frame);

            let rects = [vk::RectLayerKHR {
                offset: vk::Offset2D { x: damage.x as i32, y: damage.y as i32 },
                extent: vk::Extent2D { width: damage.w, height: damage.h },
                layer: 0,
            }];
            let region = [vk::PresentRegionKHR::default().rectangles(&rects)];
            let mut regions = vk::PresentRegionsKHR::default().regions(&region);
            let swapchains = [self.swapchain];
            let indices = [index as u32];
            let mut info = vk::PresentInfoKHR::default()
                .wait_semaphores(&signals)
                .swapchains(&swapchains)
                .image_indices(&indices);
            if self.incremental {
                info = info.push_next(&mut regions);
            }
            match self.swapchain_fn.queue_present(self.queue, &info) {
                Ok(false) => Ok(true),
                // Shown, but the window changed: rebuild before the next one.
                Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.stale = true;
                    Ok(true)
                }
                Err(e) => Err(format!("present: {e}")),
            }
        }
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        // SAFETY: the device is idle, so nothing below is in use.
        unsafe {
            let _ = self.raw.device_wait_idle();
            for s in &self.slots {
                self.raw.destroy_fence(s.fence, None);
                self.raw.destroy_semaphore(s.acquired, None);
            }
            for s in self.done.drain(..) {
                self.raw.destroy_semaphore(s, None);
            }
            self.raw.destroy_command_pool(self.pool, None);
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            }
        }
    }
}
