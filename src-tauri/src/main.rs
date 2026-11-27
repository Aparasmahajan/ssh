// No console window on Windows release builds. Without this, launching the app
// from Explorer flashes a conhost window behind the UI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Thin wiring only.
///
/// `tauri::generate_context!()` resolves `tauri.conf.json` relative to the crate
/// that invokes it, so it has to live here — that is the only reason this file is
/// not empty. Commands, state, and the handler list belong in `opspilot-app`.
fn main() {
    #[cfg(target_os = "linux")]
    webkitgtk_workarounds();

    opspilot_app::register(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("OpsPilot failed to start");
}

/// Linux renders the webview through WebKitGTK, which has one well-known
/// failure that is not ours to fix: with the **proprietary NVIDIA driver**, its
/// DMA-BUF renderer produces a blank white window (X11) or dies with
/// "Error 71 (Protocol error)" (Wayland). Every Tauri app on Linux meets this.
///
/// The fix is an environment variable that must be set before GTK initialises,
/// so it lives here, before the builder. It is applied only where the bug is:
/// on Mesa (Intel/AMD/nouveau) the DMA-BUF path works and is the faster one, and
/// while the terminal is still a webview canvas, that matters for §4's frame
/// budgets. A user who has set the variable either way keeps their choice.
#[cfg(target_os = "linux")]
fn webkitgtk_workarounds() {
    const DMABUF: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    let nvidia = std::path::Path::new("/proc/driver/nvidia/version").exists();
    if nvidia && std::env::var_os(DMABUF).is_none() {
        // Single-threaded here: nothing has been spawned yet, so mutating the
        // process environment cannot race a reader.
        std::env::set_var(DMABUF, "1");
    }
}
