import { isTauri, invoke } from "@tauri-apps/api/core";

/**
 * Which shell is this running in?
 *
 * This exists because the distinction is invisible otherwise, and it matters: in
 * a browser there is no Rust core, so there is no wgpu surface and therefore no
 * terminal at all (UI_BUILD_GUIDE Step 8 — "must NOT be an HTML element or
 * xterm.js"). A UI that looks identical in both places while one of them cannot
 * do the product's main job is a trap worth labelling.
 *
 * `isTauri()` inspects `window.__TAURI_INTERNALS__`, so it is safe to call in a
 * plain browser — it returns false rather than throwing.
 */
export const IS_DESKTOP: boolean = isTauri();

/**
 * Round-trips the one real Tauri command (`opspilot_app::app_version`) to prove
 * the IPC path works end to end. Resolves to null in a browser.
 *
 * NOT YET EXERCISED: the Rust side has never been compiled, so this call has
 * never actually run. See COMPLETED.md.
 */
export async function desktopVersion(): Promise<string | null> {
  if (!IS_DESKTOP) return null;
  try {
    return await invoke<string>("app_version");
  } catch {
    // A missing or renamed command must not take the window down with it.
    return null;
  }
}
