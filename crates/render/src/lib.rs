//! wgpu glyph atlas renderer for the terminal.
//!
//! CLAUDE.md §5.7: one 2048×2048 R8 atlas, instanced quads, one draw call per
//! frame. The pieces, in the order a frame passes through them:
//!
//!   · [`font`]: the embedded face, integer cell metrics, hinted rasterization
//!   · [`boxdraw`]: box drawing and blocks drawn to the cell, not the em box
//!   · [`atlas`]: shelf packing, the upload queue, clear-on-full
//!   · [`frame`]: grid rows → quads, rebuilt only for damaged rows (§5.3)
//!   · [`gpu`]: pipeline, atlas texture, draw and offscreen read-back
//!   · `present` (Linux): a Vulkan swapchain under wgpu that presents only
//!     the damaged rows
//!
//! Nothing here knows about windows. The caller owns the surface (a native
//! child window over the pane, docs/spikes/wgpu-compose.md) and decides when to
//! draw: on input immediately, on output once per vsync (§5.4).

pub mod atlas;
pub mod boxdraw;
pub mod font;
pub mod frame;
pub mod gpu;
pub mod palette;
#[cfg(target_os = "linux")]
pub mod present;

pub use atlas::Glyphs;
pub use font::{CellMetrics, Face, Fonts};
pub use frame::{CursorDraw, Frame, Instance, Point, Rect};
#[cfg(target_os = "linux")]
pub use present::{Need, Presenter};
pub use gpu::Renderer;
pub use palette::Palette;
pub use wgpu;
