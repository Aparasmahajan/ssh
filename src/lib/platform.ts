/** Platform-conditional modifier key.
 *
 * UI_BUILD_GUIDE writes every binding as ⌘X because it was written on a Mac.
 * On Windows and Linux the same bindings are Ctrl. Resolve it once, here, so no
 * component ever hardcodes a platform assumption — including the hint labels,
 * which must match what the key handler actually listens for.
 */
export const IS_APPLE: boolean =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform ?? "");

export const MOD_LABEL = IS_APPLE ? "⌘" : "Ctrl";
export const SHIFT_LABEL = IS_APPLE ? "⇧" : "Shift";
export const ENTER_LABEL = "⏎";

/** True when the event carries the platform's primary modifier and nothing foreign. */
export function hasMod(e: KeyboardEvent): boolean {
  return IS_APPLE ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

/** e.g. keyHint("B") -> "Ctrl B" / "⌘B" */
export function keyHint(...parts: string[]): string {
  return IS_APPLE ? parts.join("") : parts.join("+");
}

/* ---- App chords ------------------------------------------------------------ */

/**
 * The modifier for app-level shortcuts — palette, panels, close tab, copy,
 * paste.
 *
 * On macOS that is ⌘, which no terminal program ever sees. On Linux and Windows
 * it cannot be plain Ctrl, because **Ctrl+letter is a byte the shell owns**:
 *
 *   Ctrl+W  delete the previous word   (was: closed the tab)
 *   Ctrl+K  kill to end of line        (was: opened the palette)
 *   Ctrl+B  tmux prefix / back a char  (was: toggled the tree)
 *   Ctrl+I  is literally Tab, 0x09     (was: toggled the inspector)
 *   Ctrl+C  interrupt;  Ctrl+V  quoted insert
 *
 * So there every app chord adds Shift — the convention GNOME Terminal, Konsole,
 * kitty and Windows Terminal all share, and the one Linux users already have in
 * their fingers.
 */
export function isAppChord(e: KeyboardEvent, key: string): boolean {
  if (e.altKey) return false;
  const mods = IS_APPLE
    ? e.metaKey && !e.ctrlKey && !e.shiftKey
    : e.ctrlKey && e.shiftKey && !e.metaKey;
  return mods && keyIs(e, key);
}

/**
 * Does the event name this key? `key` is a letter or a `KeyboardEvent.key` name
 * like "Enter".
 *
 * A letter matches `e.key` when the layout produces a Latin letter, and the
 * physical `e.code` otherwise — so Ctrl+Shift+C still copies on a Cyrillic or
 * Greek layout, where `e.key` would be "С" or "Ψ".
 */
export function keyIs(e: KeyboardEvent, key: string): boolean {
  if (key.length !== 1) return e.key === key;
  const want = key.toLowerCase();
  const got = e.key.toLowerCase();
  if (/^[a-z]$/.test(got)) return got === want;
  return e.code === `Key${want.toUpperCase()}`;
}

/** The label for an app chord, matching [`isAppChord`]: "⌘K" / "Ctrl+Shift+K". */
export function chordHint(key: string): string {
  const k = key === "Enter" ? ENTER_LABEL : key.toUpperCase();
  return IS_APPLE ? `⌘${k}` : `Ctrl+Shift+${k}`;
}

/**
 * App shortcuts by action. One table, so a handler and its hint label cannot
 * drift apart.
 *
 * The tree is ⌘B on macOS but Ctrl+Shift+E elsewhere: Ctrl+Shift+B is already
 * broadcast-to-panes (⌘⇧B on a Mac, where the two never collide), and E is the
 * explorer chord from VS Code.
 */
export const CHORD = {
  palette: "k",
  tree: IS_APPLE ? "b" : "e",
  inspector: "i",
  zoom: "Enter",
  closeTab: "w",
  copy: "c",
  paste: "v",
} as const;
