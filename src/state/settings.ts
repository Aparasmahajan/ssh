import { create } from "zustand";
import { load, save } from "@/lib/persist";

/**
 * User settings that are not per-workspace.
 *
 * §0.8 lists these explicitly: "Terminal font size, family, and line height are
 * user settings." Font size in particular is the one people change constantly —
 * plugging into a 4K monitor, or reading a log at arm's length — so it gets
 * first-class keybindings rather than living in a settings dialog.
 *
 * Stored under the machine-local prefix, not the portable config: §12.1 puts
 * display preferences in the "DO NOT COPY. All rebuildable" half, because a
 * font size that suits a laptop panel is wrong on the desktop you sync to.
 */

/** Below ~8px a monospace face stops being legible; above ~32px a terminal
 *  holds too few columns to be useful. Both ends are deliberate. */
export const MIN_FONT_PX = 8;
export const MAX_FONT_PX = 32;
export const DEFAULT_FONT_PX = 12.5;

/** Line height as a multiple of font size.
 *
 *  12.5px → 17px in the original constants, so 1.36. Scaling the ratio rather
 *  than keeping 17px fixed is what stops large text overlapping and small text
 *  floating in a sea of leading. */
export const LINE_HEIGHT_RATIO = 1.36;

const KEY = "settings";

interface Persisted {
  terminalFontPx: number;
}

interface SettingsState extends Persisted {
  lineHeightPx: () => number;
  setTerminalFontPx: (px: number) => void;
  adjustFont: (delta: number) => void;
  resetFont: () => void;
}

/** Snap to a quarter pixel: finer than that produces no visible change but does
 *  force a PTY resize round trip on every keypress. */
const clamp = (px: number) =>
  Math.min(MAX_FONT_PX, Math.max(MIN_FONT_PX, Math.round(px * 4) / 4));

const initial = load<Partial<Persisted>>(KEY, {});

export const useSettings = create<SettingsState>((set, get) => ({
  terminalFontPx: clamp(initial.terminalFontPx ?? DEFAULT_FONT_PX),

  lineHeightPx: () => Math.round(get().terminalFontPx * LINE_HEIGHT_RATIO),

  setTerminalFontPx: (px) => {
    const terminalFontPx = clamp(px);
    set({ terminalFontPx });
    save(KEY, { terminalFontPx });
  },

  // A fixed step rather than a percentage: at 10px a 10% step is invisible, and
  // at 28px it is a jump of three.
  adjustFont: (delta) => get().setTerminalFontPx(get().terminalFontPx + delta),

  resetFont: () => get().setTerminalFontPx(DEFAULT_FONT_PX),
}));

/**
 * Ctrl/⌘ with `+`, `-` and `0` — the binding every terminal and browser uses.
 *
 * Handles the awkward parts of the real keyboard: `+` usually arrives as
 * `Shift+=`, and the numpad sends `Add`/`Subtract`. Missing those is why
 * zoom shortcuts feel broken on some layouts.
 */
export function isFontZoomKey(e: KeyboardEvent): "in" | "out" | "reset" | null {
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return null;
  switch (e.key) {
    case "+":
    case "=":
    case "Add":
      return "in";
    case "-":
    case "_":
    case "Subtract":
      return "out";
    case "0":
      return "reset";
    default:
      return null;
  }
}
