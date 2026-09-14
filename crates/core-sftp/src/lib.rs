//! Pipelined SFTP engine with AIMD window and tar-pipe fallback.
//!
//! CLAUDE.md §6, the flagship win. §6.1 treat outstanding requests as a
//! congestion window; §6.2 when the queue is >200 files averaging <1MB, stop
//! tuning SFTP and switch protocol — stream `tar -cf -` over an exec channel.
//!
//! STATUS: skeleton. No Rust toolchain was available when this crate was
//! created, so it has never been compiled. See COMPLETED.md.

/// Placeholder so the crate has a compilable surface and `cargo build` has
/// something to check. Delete when real types land.
pub const CRATE_NAME: &str = "opspilot-core-sftp";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert!(!super::CRATE_NAME.is_empty());
    }
}
