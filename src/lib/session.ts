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
