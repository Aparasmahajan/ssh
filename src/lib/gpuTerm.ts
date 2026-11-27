import { invoke } from "@tauri-apps/api/core";

/**
 * The native GPU terminal (`crates/app/src/gpu_term.rs`).
 *
 * A pane reports its rect, focus, selection and whether an overlay covers it;
 * Rust keeps a native child window over the rect and draws the terminal there.
 * The child window is click-through, so mouse input stays with the page.
 * Keys do not: while a GPU pane has focus, Rust encodes and writes them before
 * WebKit sees them, and only app chords reach the page.
 * While an overlay covers the pane, the child hides and the pane's own canvas
 * shows, so the overlay stays on top.
 */

export interface PaneState {
  pane: string;
  session: number;
  /** CSS px, relative to the webview. */
  rect: { x: number; y: number; w: number; h: number };
  dpr: number;
  font_px: number;
  line_px: number;
  visible: boolean;
  focused: boolean;
  /** Viewport cells `[[row, col], [row, col]]`, inclusive. */
  selection: [[number, number], [number, number]] | null;
  /** Sessions keystrokes go to: this one, or the whole tab while broadcasting.
   *  Rust types into these directly while this pane has focus. */
  targets: number[];
}

/** Emitted by Rust after it typed a key into `pane` natively (the page never
 *  saw the key). Payload: the pane id. */
export const NATIVE_KEY_EVENT = "term-native-key";

export interface PaneInfo {
  /** Cell size in CSS px. Lay the grid out with these while the GPU path is on. */
  cell_w: number;
  cell_h: number;
  /** The child window is showing and drawn into. Until then, paint the canvas. */
  live: boolean;
}

/** Set once any call fails: the platform has no GPU path, stop asking. */
let unavailable = false;

export const gpuAvailable = () => !unavailable;

export async function gpuPaneSet(state: PaneState): Promise<PaneInfo | null> {
  if (unavailable) return null;
  try {
    return await invoke<PaneInfo>("gpu_pane_set", { state });
  } catch {
    unavailable = true;
    return null;
  }
}

export const gpuPaneRemove = (pane: string) => invoke<void>("gpu_pane_remove", { pane }).catch(() => undefined);

/** Overlays mark themselves with `data-overlay`; any that intersects the pane
 *  hides the native view. O(overlays), and there are a handful at most. */
export function coveredByOverlay(rect: DOMRect): boolean {
  for (const el of document.querySelectorAll("[data-overlay]")) {
    const r = el.getBoundingClientRect();
    if (r.width > 0 && r.height > 0 && r.left < rect.right && r.right > rect.left && r.top < rect.bottom && r.bottom > rect.top) {
      return true;
    }
  }
  return false;
}
