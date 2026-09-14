// No console window on Windows release builds. Without this, launching the app
// from Explorer flashes a conhost window behind the UI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Thin wiring only.
///
/// `tauri::generate_context!()` resolves `tauri.conf.json` relative to the crate
/// that invokes it, so it has to live here — that is the only reason this file is
/// not empty. Commands, state, and the handler list belong in `opspilot-app`.
fn main() {
    opspilot_app::register(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("OpsPilot failed to start");
}
