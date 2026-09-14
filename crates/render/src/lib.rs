//! wgpu glyph atlas renderer.
//!
//! CLAUDE.md §5.7 and UI_BUILD_GUIDE Step 8. One 2048×2048 R8 atlas,
//! instanced quads in a persistent mapped buffer, one draw call per frame.
//! §5.4: input renders immediately, output coalesces at vsync — splitting those
//! paths is the PuTTY fix. Ligatures off in the terminal.
//!
//! STATUS: skeleton. No Rust toolchain was available when this crate was
//! created, so it has never been compiled. See COMPLETED.md.

/// Placeholder so the crate has a compilable surface and `cargo build` has
/// something to check. Delete when real types land.
pub const CRATE_NAME: &str = "opspilot-render";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert!(!super::CRATE_NAME.is_empty());
    }
}
