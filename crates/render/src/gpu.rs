//! The wgpu side: one pipeline, one atlas texture, one instance buffer.
//!
//! **One colour pipeline for every target.** Render targets use a non-sRGB
//! format and blending happens on the sRGB-encoded values, which is what the
//! webview's canvas and cairo do. The spike drew the direct path into an sRGB
//! swapchain and the overlay path into `Bgra8Unorm`, and the terminal changed
//! brightness every time a dialog opened (docs/spikes/wgpu-compose.md).
//! [`Renderer::surface_format`] picks the non-sRGB format for a surface.

use crate::atlas::{Glyphs, ATLAS_SIZE};
use crate::frame::{Instance, Rect, KIND_SOLID};
use crate::palette::rgba;

/// Output alpha stays 1 over an opaque clear: colour blends normally, alpha
/// accumulates as `a + dst·(1−a)`. With ordinary alpha blending the alpha
/// channel would come out as `a² + 1 − a` under every glyph edge, and the
/// overlay path, which hands these pixels to cairo as premultiplied ARGB, would
/// show the webview through the text.
const BLEND: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::SrcAlpha,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    atlas: wgpu::Texture,
    instances: wgpu::Buffer,
    /// In instances.
    capacity: usize,
    format: wgpu::TextureFormat,
    /// Atlas epoch the texture's contents belong to.
    epoch: u32,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Renderer {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("term"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("term uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: ATLAS_SIZE, height: ATLAS_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("term"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let atlas_view = atlas.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("term"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("term"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Uint32, 4 => Uint32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("term"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(BLEND),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let capacity = 4096;
        Renderer {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            bind,
            uniforms,
            atlas,
            instances: instance_buffer(device, capacity),
            capacity,
            format,
            epoch: 0,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// The surface format to configure: the surface's preferred one with the
    /// sRGB encoding removed, so the swapchain blends like the offscreen path.
    pub fn surface_format(caps: &wgpu::SurfaceCapabilities) -> Option<wgpu::TextureFormat> {
        let linear = |f: &wgpu::TextureFormat| !f.is_srgb();
        caps.formats.iter().copied().find(linear).or_else(|| caps.formats.first().copied())
    }

    /// Send newly rasterized glyphs to the atlas texture.
    pub fn upload(&mut self, glyphs: &mut Glyphs) {
        self.epoch = glyphs.epoch;
        for up in glyphs.take_uploads() {
            let s = up.slot;
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: s.x as u32, y: s.y as u32, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &up.data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(s.w as u32), rows_per_image: Some(s.h as u32) },
                wgpu::Extent3d { width: s.w as u32, height: s.h as u32, depth_or_array_layers: 1 },
            );
        }
    }

    /// Record one frame: clear to `bg` (`0xRRGGBB`), then every instance in
    /// one draw call.
    pub fn draw(&mut self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, size: (u32, u32), bg: u32, instances: &[Instance]) {
        self.draw_damage(enc, target, size, bg, instances, None);
    }

    /// Like [`Renderer::draw`], but with `region` only that rectangle of
    /// `target` is touched: its previous contents load, the region is filled
    /// with `bg`, and every instance draws scissored to it. The rest of the
    /// target must already hold the current frame. For a keystroke that is one
    /// band of rows instead of the whole surface (CLAUDE.md §5.3).
    pub fn draw_damage(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        bg: u32,
        instances: &[Instance],
        region: Option<Rect>,
    ) {
        let region = region.and_then(|r| r.clip(size));
        // Slot 0 is the band's background, drawn only for a partial frame.
        let n = instances.len() + 1;
        if n > self.capacity {
            self.capacity = n.next_power_of_two();
            self.instances = instance_buffer(&self.device, self.capacity);
        }
        let stride = std::mem::size_of::<Instance>() as u64;
        if let Some(r) = region {
            let fill = Instance {
                pos: [r.x as f32, r.y as f32],
                size: [r.w as f32, r.h as f32],
                uv: [0.0, 0.0],
                color: rgba(bg, 255),
                kind: KIND_SOLID,
            };
            self.queue.write_buffer(&self.instances, 0, bytemuck::bytes_of(&fill));
        }
        if !instances.is_empty() {
            self.queue.write_buffer(&self.instances, stride, bytemuck::cast_slice(instances));
        }
        let mut u = [0u8; 16];
        u[0..4].copy_from_slice(&(size.0.max(1) as f32).to_le_bytes());
        u[4..8].copy_from_slice(&(size.1.max(1) as f32).to_le_bytes());
        self.queue.write_buffer(&self.uniforms, 0, &u);

        let c = |shift: u32| ((bg >> shift) & 0xFF) as f64 / 255.0;
        let load = match region {
            Some(_) => wgpu::LoadOp::Load,
            None => wgpu::LoadOp::Clear(wgpu::Color { r: c(16), g: c(8), b: c(0), a: 1.0 }),
        };
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("term"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        let first = if region.is_some() { 0 } else { 1 };
        if (first..n as u32).is_empty() {
            return;
        }
        if let Some(r) = region {
            pass.set_scissor_rect(r.x, r.y, r.w, r.h);
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..n as u64 * stride));
        pass.draw(0..4, first..n as u32);
    }

    /// Render to a texture and read it back as tightly packed rows of the
    /// renderer's format (BGRA for `Bgra8Unorm`). The overlay path and the
    /// tests use this; it waits for the GPU. `None` if the buffer could not
    /// be mapped (device lost).
    pub fn read_back(&mut self, size: (u32, u32), bg: u32, instances: &[Instance]) -> Option<Vec<u8>> {
        let (w, h) = (size.0.max(1), size.1.max(1));
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("term offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("term readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = tex.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.draw(&mut enc, &view, (w, h), bg, instances);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: Default::default(), aspect: Default::default() },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        let mapped = buf.slice(..).get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in mapped.chunks(padded as usize).take(h as usize) {
            out.extend_from_slice(&row[..(w * 4) as usize]);
        }
        Some(out)
    }
}

fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("term instances"),
        size: (capacity * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// An adapter and device that can present to `surface`. `None` if there is
/// none (no driver, or a driver that cannot present to this window).
pub fn for_surface(instance: &wgpu::Instance, surface: &wgpu::Surface<'_>) -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    for_surface_damage(instance, surface, false).map(|(a, d, q, _)| (a, d, q))
}

/// [`for_surface`], and with `damage` on Linux/Vulkan, a device the
/// damage-only presenter (`present`) can use. The last value is then
/// `Some(incremental)`: whether presents can name the changed region. `None`
/// means use wgpu's own surface.
pub fn for_surface_damage(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'_>,
    damage: bool,
) -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue, Option<bool>)> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(surface),
        ..Default::default()
    }))
    .ok()?;
    #[cfg(target_os = "linux")]
    if damage && adapter.get_info().backend == wgpu::Backend::Vulkan {
        if let Some((device, queue, incremental)) = crate::present::open_device(&adapter) {
            return Some((adapter, device, queue, Some(incremental)));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = damage;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some((adapter, device, queue, None))
}

/// A device with no surface, for tests and offscreen rendering. `None` where
/// there is no adapter at all (a CI box with no GPU and no software Vulkan).
pub fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}
