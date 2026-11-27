/**
 * Keyboard event -> the bytes a terminal expects (xterm encoding).
 *
 * Returns null when the key is not ours, so the browser keeps its default —
 * never swallow a key you do not handle, or the user loses devtools, zoom, and
 * the shell keybindings the app itself defines.
 *
 * App chords (palette, tabs, copy/paste) never get here: they are claimed by
 * capture-phase listeners that stop propagation first. That is why this file no
 * longer carves out Ctrl+B/I/K — on Linux and Windows those are the shell's
 * (see `isAppChord` in platform.ts).
 *
 * Bytes, not a string: Alt-modified keys and raw control sequences are not
 * always valid UTF-8, and a round-trip through a JS string would corrupt them.
 */

const ENC = new TextEncoder();

/** Input modes the application has set; from the latest snapshot. */
export interface KeyModes {
  /** DECCKM — cursor keys as `ESC O x`. ncurses turns this on at startup. */
  appCursor: boolean;
}

const NORMAL: KeyModes = { appCursor: false };

/** Cursor-style keys: final byte of `CSI x` / `SS3 x` / `CSI 1;m x`. */
const CURSOR: Record<string, string> = {
  ArrowUp: "A",
  ArrowDown: "B",
  ArrowRight: "C",
  ArrowLeft: "D",
  Home: "H",
  End: "F",
};

/** F1–F4 are SS3 keys: `ESC O P`, or `CSI 1;m P` when modified. */
const SS3_FN: Record<string, string> = { F1: "P", F2: "Q", F3: "R", F4: "S" };

/** `CSI n ~` keys, or `CSI n;m ~` when modified. */
const TILDE: Record<string, number> = {
  Insert: 2,
  Delete: 3,
  PageUp: 5,
  PageDown: 6,
  F5: 15,
  F6: 17,
  F7: 18,
  F8: 19,
  F9: 20,
  F10: 21,
  F11: 23,
  F12: 24,
};

/** xterm's modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4). 1 means none. */
function modParam(e: KeyboardEvent): number {
  return 1 + (e.shiftKey ? 1 : 0) + (e.altKey ? 2 : 0) + (e.ctrlKey ? 4 : 0);
}

/** Ctrl+punctuation control codes. Ctrl+digits and Ctrl+- are absent on
 *  purpose: tab switching and font zoom claim them before this runs. */
const CTRL_PUNCT: Record<string, number> = {
  "@": 0x00,
  " ": 0x00,
  "[": 0x1b,
  "\\": 0x1c,
  "]": 0x1d,
  "^": 0x1e,
  _: 0x1f, // readline undo
  "/": 0x1f,
  "?": 0x7f,
};

export function keyToBytes(e: KeyboardEvent, modes: KeyModes = NORMAL): Uint8Array | null {
  // An IME is mid-composition: the key belongs to it, not to the shell.
  if (e.isComposing || e.keyCode === 229) return null;
  if (e.metaKey) return null; // ⌘ / Super: leave OS shortcuts alone

  const esc = (s: string) => ENC.encode(e.altKey ? "\x1b" + s : s);
  const m = modParam(e);

  const cur = CURSOR[e.key];
  if (cur !== undefined) {
    // Modified cursor keys always use the CSI form, whatever DECCKM says —
    // Ctrl+Left is `ESC[1;5D`, which is what readline and zsh bind to
    // backward-word.
    if (m > 1) return ENC.encode(`\x1b[1;${m}${cur}`);
    return ENC.encode(modes.appCursor ? `\x1bO${cur}` : `\x1b[${cur}`);
  }

  const fn = SS3_FN[e.key];
  if (fn !== undefined) {
    return ENC.encode(m > 1 ? `\x1b[1;${m}${fn}` : `\x1bO${fn}`);
  }

  const tilde = TILDE[e.key];
  if (tilde !== undefined) {
    return ENC.encode(m > 1 ? `\x1b[${tilde};${m}~` : `\x1b[${tilde}~`);
  }

  switch (e.key) {
    case "Enter":
      return esc("\r");
    case "Escape":
      return esc("\x1b");
    case "Tab":
      // Shift+Tab is back-tab, which completion menus and forms rely on.
      return e.shiftKey ? ENC.encode("\x1b[Z") : esc("\t");
    case "Backspace":
      // DEL, as every Linux terminal sends; Ctrl+Backspace sends BS, which
      // most setups bind to delete-word.
      return e.ctrlKey ? new Uint8Array([0x08]) : esc("\x7f");
  }

  if (e.key.length !== 1) return null; // Shift, Control, dead keys, …

  // AltGr produces a character, not a chord. Windows reports it as Ctrl+Alt,
  // so without this check `@` on a German layout (AltGr+Q) hit the Ctrl branch
  // below, matched no control code, and was dropped.
  if (e.getModifierState?.("AltGraph")) return ENC.encode(e.key);

  if (e.ctrlKey) {
    // Ctrl+A..Z -> 0x01..0x1A, with or without Shift (as xterm does). The
    // physical key decides on non-Latin layouts, so Ctrl+C still interrupts
    // with a Cyrillic layout active.
    let c = e.key.toLowerCase();
    if (!/^[a-z]$/.test(c) && /^Key[A-Z]$/.test(e.code)) c = e.code.slice(3).toLowerCase();
    let byte: number | undefined;
    if (c >= "a" && c <= "z" && c.length === 1) byte = c.charCodeAt(0) - 96;
    else byte = CTRL_PUNCT[e.key];
    if (byte === undefined) return null;
    return e.altKey ? new Uint8Array([0x1b, byte]) : new Uint8Array([byte]);
  }

  // Alt+key sends ESC first ("meta sends escape").
  return esc(e.key);
}
