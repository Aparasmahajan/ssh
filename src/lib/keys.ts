/**
 * Keyboard event -> the bytes a terminal expects.
 *
 * Returns null when the key is not ours, so the browser keeps its default —
 * never swallow a key you do not handle, or the user loses devtools, zoom, and
 * the shell keybindings the app itself defines.
 *
 * Bytes, not a string: Alt-modified keys and raw control sequences are not
 * always valid UTF-8, and a round-trip through a JS string would corrupt them.
 */

const ENC = new TextEncoder();

/** DECCKM-agnostic: normal cursor keys. Applications that want the application
 *  variant send DECCKM and the shell handles it; this is the common default. */
const SPECIAL: Record<string, string> = {
  Enter: "\r",
  Tab: "\t",
  Backspace: "\x7f",
  Escape: "\x1b",
  Delete: "\x1b[3~",
  ArrowUp: "\x1b[A",
  ArrowDown: "\x1b[B",
  ArrowRight: "\x1b[C",
  ArrowLeft: "\x1b[D",
  Home: "\x1b[H",
  End: "\x1b[F",
  PageUp: "\x1b[5~",
  PageDown: "\x1b[6~",
  Insert: "\x1b[2~",
  F1: "\x1bOP",
  F2: "\x1bOQ",
  F3: "\x1bOR",
  F4: "\x1bOS",
  F5: "\x1b[15~",
  F6: "\x1b[17~",
  F7: "\x1b[18~",
  F8: "\x1b[19~",
  F9: "\x1b[20~",
  F10: "\x1b[21~",
  F11: "\x1b[23~",
  F12: "\x1b[24~",
};

export function keyToBytes(e: KeyboardEvent): Uint8Array | null {
  // Leave the shell's own bindings alone — AppShell owns Ctrl+B/I/Enter, and
  // the command palette owns Ctrl+K.
  if ((e.ctrlKey || e.metaKey) && !e.altKey) {
    const k = e.key.toLowerCase();
    if (k === "b" || k === "i" || k === "k" || e.key === "Enter") return null;
  }

  const special = SPECIAL[e.key];
  if (special !== undefined) {
    // Alt+key sends ESC followed by the key ("meta sends escape").
    return ENC.encode(e.altKey ? "\x1b" + special : special);
  }

  if (e.key.length !== 1) return null; // Shift, Control, dead keys, IME

  // Ctrl+A..Z -> 0x01..0x1A, plus the classic Ctrl+[ \ ] ^ _ punctuation.
  if (e.ctrlKey && !e.metaKey) {
    const c = e.key.toLowerCase();
    if (c >= "a" && c <= "z") {
      return new Uint8Array([c.charCodeAt(0) - 96]);
    }
    const punct: Record<string, number> = {
      "[": 0x1b,
      "\\": 0x1c,
      "]": 0x1d,
      "^": 0x1e,
      _: 0x1f,
      " ": 0x00,
      "?": 0x7f,
    };
    const b = punct[e.key];
    if (b !== undefined) return new Uint8Array([b]);
    return null;
  }

  if (e.metaKey) return null; // leave OS shortcuts alone

  return ENC.encode(e.altKey ? "\x1b" + e.key : e.key);
}
