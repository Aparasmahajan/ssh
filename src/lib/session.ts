import { invoke } from "@tauri-apps/api/core";

/**
 * Typed wrapper over the session commands in `crates/app`.
 *
 * The shape mirrors the Rust side exactly: `snapshot` returns only rows that
 * changed, and `null` when nothing changed at all. That is what makes polling at
 * 60 Hz affordable — an idle terminal costs one atomic load and no serialization
 * (CLAUDE.md §13.1 rule 8: sample, never stream).
 */

export interface Span {
  start: number;
  len: number;
  fg: string;
  bg: string;
  bold: boolean;
  italic: boolean;
  underline: boolean;
  reverse: boolean;
}

export interface LineUpdate {
  row: number;
  text: string;
  spans: Span[];
}

export interface Snapshot {
  generation: number;
  rows: number;
  cols: number;
  cursor_row: number;
  cursor_col: number;
  title: string;
  alive: boolean;
  lines: LineUpdate[];
  /** Discard what you have and repaint everything (first frame, resize, scroll). */
  full: boolean;
  /** Lines scrolled back from the live viewport. 0 = at the bottom. */
  scroll_offset: number;
  /** How far back scrolling can go. */
  scroll_max: number;
  /** DECCKM: arrows must be sent as `ESC O A`, not `ESC [ A`. */
  app_cursor: boolean;
  /** DECTCEM. False while a full-screen program has hidden the cursor. */
  cursor_visible: boolean;
}

/** A shell this machine can actually start. */
export interface ShellInfo {
  id: string;
  label: string;
  program: string;
  args: string[];
}

export const openSession = (rows: number, cols: number, shell?: string) =>
  invoke<number>("session_open", shell === undefined ? { rows, cols } : { rows, cols, shell });

export const writeSession = (id: number, bytes: Uint8Array) =>
  invoke<void>("session_write", { id, bytes: Array.from(bytes) });

export const snapshotSession = (id: number, since: number) =>
  invoke<Snapshot | null>("session_snapshot", { id, since });

export const resizeSession = (id: number, rows: number, cols: number) =>
  invoke<void>("session_resize", { id, rows, cols });

export const scrollSession = (id: number, delta: number) =>
  invoke<number>("session_scroll", { id, delta });

export const closeSession = (id: number) => invoke<void>("session_close", { id });

export const listShells = () => invoke<ShellInfo[]>("list_shells");

/* ---- Clipboard ------------------------------------------------------------- */

/** `primary` is the X11/Wayland select-to-copy, middle-click-to-paste buffer. */
export type ClipboardSelection = "clipboard" | "primary";

/** The exact error the Rust side returns where it has no native clipboard. */
const UNSUPPORTED = "clipboard: unsupported";

/**
 * Paste into these sessions.
 *
 * On Linux the clipboard is read in Rust and written straight to the PTY — the
 * text never comes back over IPC, so a compromised webview cannot read whatever
 * password the user last copied (CLAUDE.md §18.14). Elsewhere the native side
 * declines and this falls back to the web clipboard API; Rust still decides
 * line endings and bracketed paste either way.
 */
export async function pasteInto(ids: number[], selection: ClipboardSelection): Promise<void> {
  if (ids.length === 0) return;
  try {
    await invoke<void>("session_paste", { ids, selection });
  } catch (e) {
    if (String(e) !== UNSUPPORTED) throw e;
    // PRIMARY is an X11/Wayland concept; there is nothing to fall back to.
    if (selection === "primary") return;
    const text = await navigator.clipboard.readText();
    await invoke<void>("session_paste_text", { ids, text });
  }
}

/** Copy text out of the terminal. PRIMARY is a no-op where it does not exist. */
export async function copyText(text: string, selection: ClipboardSelection): Promise<void> {
  if (text === "") return;
  try {
    await invoke<void>("clipboard_write", { selection, text });
  } catch (e) {
    if (String(e) !== UNSUPPORTED) throw e;
    if (selection === "clipboard") await navigator.clipboard.writeText(text);
  }
}
