//! Terminal engine: SoA grid, VT500 DFA parser, ring scrollback.
//!
//! CLAUDE.md §17 build order step 1 — the performance spine. §5 in full.
//!
//! What is here:
//!   · [`grid`] — structure-of-arrays cells, 6 bytes each, O(1) damage bitset (§5.1, §5.3)
//!   · [`style`] — style interning, the reason a cell is 6 bytes and not 16 (§5.1)
//!   · [`parser`] — table-driven VT500 DFA with a SWAR ASCII bulk path (§5.2)
//!   · [`scrollback`] — fixed-capacity ring, variable-length rows (§5.6)
//!   · [`screen`] — the two joined: a `Perform` that drives a grid
//!
//!   · [`width`] — East Asian / emoji display width (§5.1 wide cells)
//!   · [`predict`] — predictive local echo and its hit-rate instrumentation (§5.5)
//!
//! Deliberately NOT here yet:
//!   · Reflow on resize (§5.6). Needs logical-line tracking above the grid.
//!   · Rendering. That is `opspilot-render`, which reads these planes directly.

pub mod grid;
pub mod params;
pub mod parser;
pub mod predict;
pub mod screen;
pub mod scrollback;
pub mod style;
pub mod width;

pub use grid::{Cursor, Grid};
pub use params::Params;
pub use parser::{Parser, Perform};
pub use predict::{Prediction, Predictor};
pub use screen::Screen;
pub use scrollback::{Row, Scrollback};
pub use style::{Style, StyleId, StyleTable};
pub use width::char_width;
