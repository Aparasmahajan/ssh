import { useCallback, useEffect, useRef, useState } from "react";
import { keyToBytes, type KeyModes } from "@/lib/keys";
import { CHORD, isAppChord } from "@/lib/platform";
import {
  copyText,
  listShells,
  openSession,
  pasteInto,
  resizeSession,
  scrollSession,
  snapshotSession,
  writeSession,
  type ClipboardSelection,
  type LineUpdate,
  type ShellInfo,
} from "@/lib/session";
import { sshOpenShell, type ConnectionInfo } from "@/lib/remote";
import {
  coveredByOverlay,
  gpuAvailable,
  gpuPaneRemove,
  gpuPaneSet,
  NATIVE_KEY_EVENT,
  type PaneState,
} from "@/lib/gpuTerm";
import { listen } from "@tauri-apps/api/event";
import { isReadOnly, openStream, streamTitle, type StreamSource } from "@/lib/streams";
import { sessionsFor } from "@/lib/broadcast";
import { acquire, keyOf, release } from "@/state/terminals";
import { leaves, usePanes } from "@/state/panes";
import { useSettings } from "@/state/settings";

/**
 * The terminal surface.
 *
 * ## This is a canvas, and CLAUDE.md says it should not be
 *
 * §3 is unambiguous: "Anything updating faster than 4 Hz is a wgpu surface", and
 * UI_BUILD_GUIDE Step 8 says the terminal "must NOT be an HTML element or
 * xterm.js". This is a 2D canvas in the webview, which is neither wgpu nor
 * compliant. It is an **interim** so the app is usable while `crates/render`
 * is written; recorded as D-19 in docs/DECISIONS.md with what it costs.
 *
 * What it does keep from §5, because these are the parts that survive the port:
 *
 *   · **Damage-driven.** Only rows the Rust side reports as changed are redrawn.
 *     The canvas is never cleared wholesale except on a full repaint.
 *   · **Coalesced at vsync (§5.4).** One snapshot per animation frame, however
 *     many thousands of PTY writes arrived. The Rust side bumps a counter; this
 *     samples it.
 *   · **No per-byte IPC.** The webview never sees the byte stream.
 *
 * What it cannot do, and is the reason it is temporary: the glyphs are rasterised
 * by the browser on the UI thread, so a flood competes with React for the same
 * thread — exactly the coupling §3 exists to forbid.
 *
 * ## The GPU path (Linux/X11)
 *
 * Where `gpu_pane_set` succeeds, Rust draws this pane in a native child window
 * over the canvas (`crates/app/src/gpu_term.rs`) and the canvas stops painting.
 * This component still owns input, selection and layout, and reports the
 * pane's rect, focus and selection once per frame when they change. While an
 * overlay (`data-overlay`) covers the pane the native view hides and the
 * canvas, kept current underneath, paints again. Cell metrics then come from
 * Rust (whole pixels), so both paths lay out the same grid.
 *
 * Keys: while a GPU pane has focus, Rust types into the session itself and the
 * page only sees app chords (`crates/app/src/keys.rs`). `onKeyDown` below is
 * then the path for those chords, and the whole path on the canvas.
 */

const PADDING = 6;

/** Fire-and-forget IPC. A keystroke into an exited session, or a resize racing
 *  a close, fails harmlessly; it must not raise an unhandled rejection per key. */
const quiet = (p: Promise<unknown>) => void p.catch(() => undefined);

type Cell = { w: number; h: number };

/** A cell in viewport coordinates. */
type Point = { row: number; col: number };
/** Anchor and head, in the order the user made them; see `ordered`. */
type Selection = { a: Point; b: Point };

/** Selection is drawn translucent OVER the text, so the glyphs stay readable. */
const SELECTION_ALPHA = 0.32;

/** Start and end of a selection in reading order. */
function ordered(s: Selection): [Point, Point] {
  const { a, b } = s;
  return a.row < b.row || (a.row === b.row && a.col <= b.col) ? [a, b] : [b, a];
}

/**
 * Characters a double-click keeps inside one word. Deliberately broad — `/`,
 * `.`, `-`, `:`, `@`, `~`, `=` — so a double-click takes a whole path, URL,
 * `user@host` or `KEY=value`, which is what one is double-clicked for in a
 * terminal. Quotes, brackets and shell operators end a word.
 */
const WORD_CHAR = /[^\s"'`()[\]{}<>|;&]/;

/** [first, last] column of the word at `col`. O(row length). */
function wordAt(cells: readonly string[], col: number): [number, number] {
  if (!WORD_CHAR.test(cells[col] ?? " ")) return [col, col];
  let lo = col;
  let hi = col;
  while (lo > 0 && WORD_CHAR.test(cells[lo - 1] ?? " ")) lo--;
  while (hi < cells.length - 1 && WORD_CHAR.test(cells[hi + 1] ?? " ")) hi++;
  return [lo, hi];
}

/**
 * Resolve `--font-term` to a literal family list.
 *
 * `ctx.font` parses a CSS font shorthand with NO element context, so `var()`
 * never resolves and the whole assignment is **silently ignored** — the context
 * keeps its default `10px sans-serif`. That produced a terminal drawn in a
 * proportional face while cell width was measured from the same broken font, so
 * `col * cellWidth` and the actual glyph advances disagreed and the cursor drew
 * in the wrong place. Read the computed value and hand canvas a real family.
 */
function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function termFamily(): string {
  // No hardcoded fallback family: if the token is missing that is a real
  // regression and should look wrong, not be papered over. The ESLint no-hex
  // rule enforces the same principle for colours.
  return cssVar("--font-term");
}

/** Font shorthand for a span. Weight and slant vary; family never does. */
function fontFor(family: string, px: number, bold: boolean, italic: boolean): string {
  return `${italic ? "italic " : ""}${bold ? 700 : 400} ${px}px ${family}`;
}

export function TerminalSurface({
  active,
  connection,
  source = null,
  onConnect,
  onDisconnect,
  paneId,
  tabId,
  broadcast = false,
}: {
  active: boolean;
  /** When set, the session is a shell on this host instead of a local one. */
  connection?: ConnectionInfo | null;
  /** When set, the session is a container/pod exec or a log follow. */
  source?: StreamSource | null;
  onConnect?: () => void;
  onDisconnect?: () => void;
  /** Identifies this pane in a split layout; absent when unsplit. */
  paneId?: string;
  /** Which tab this pane belongs to, for broadcast. */
  tabId?: string;
  /** Every keystroke also goes to the other panes in this tab (Step 9). */
  broadcast?: boolean;
}) {
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  /** Selection layer. Separate so a damaged row repaint never erases the
   *  highlight, and drawing the highlight never forces a text repaint. */
  const overlayRef = useRef<HTMLCanvasElement | null>(null);
  const selection = useRef<Selection | null>(null);
  /** Removes the window listeners of a drag in progress. */
  const endDrag = useRef<(() => void) | null>(null);
  const selColor = useRef<string>("");
  /** Input modes from the latest snapshot — DECCKM changes what arrows send. */
  const modes = useRef<KeyModes>({ appCursor: false });
  const cursorVisible = useRef(true);

  const sessionId = useRef<number | null>(null);
  const generation = useRef(0);
  /**
   * Fetch a snapshot now rather than at the next animation frame. §5.4's input
   * path: a keystroke renders on the input event, not up to 16 ms later. With
   * predictive echo (§5.5) the predicted character is already in the snapshot
   * by the time the write resolves, so this is what makes it appear instantly.
   */
  const pollNow = useRef<() => void>(() => {});
  /** Latest text for every row, so a resize or DPI change can repaint without
   *  asking Rust to resend content it already sent. */
  const lines = useRef<Map<number, LineUpdate>>(new Map());
  const cursor = useRef({ row: 0, col: 0 });
  /** Where the amber block was last drawn, so it can be erased. Without this the
   *  cursor leaves a trail: painting a row does not clear a block sitting on a
   *  row that did not change. */
  const paintedCursor = useRef<{ row: number; col: number } | null>(null);
  const cellRef = useRef<Cell>({ w: 8, h: 17 });
  /** Resolved once; `getComputedStyle` per span would be a layout read per row. */
  /** Resolved once at mount; `getComputedStyle` per span would be a layout read
   *  per row. Empty until then — nothing paints before the mount effect runs. */
  const familyRef = useRef<string>("");
  const cursorColor = useRef<string>("");
  const bgColor = useRef<string>("");
  const gridRef = useRef({ rows: 24, cols: 80 });
  /** GPU path state. `live`: the native view is drawing; `covered`: an
   *  overlay hides it, so the canvas paints instead. */
  const gpu = useRef({ live: false, covered: false, inFlight: false, again: false, sent: "" });
  /** Send the pane state now rather than at the next frame (focus changes). */
  const syncNow = useRef<() => void>(() => {});
  /** Broadcast settings, read from the per-frame sync without re-running it. */
  const bcast = useRef({ broadcast, tabId });
  bcast.current = { broadcast, tabId };
  /** Cell size from Rust while the GPU path is on, in CSS px. */
  const gpuCell = useRef<Cell | null>(null);

  const [error, setError] = useState<string | null>(null);
  const [exited, setExited] = useState(false);
  const [shells, setShells] = useState<ShellInfo[]>([]);
  // A pane that is remounting (tab switch, split) restores the shell it was
  // running rather than falling back to the default and reopening.
  const [shellId, setShellId] = useState<string | null>(() => {
    const k = paneId ? keyOf(paneId) : null;
    return k?.startsWith("local:") ? k.slice(6) : null;
  });
  /** Panes without an id (none today) own their session for the component's
   *  lifetime; panes with one hand it to state/terminals.ts. */
  const soloPane = useRef(`solo-${Math.random().toString(36).slice(2)}`);
  const pane = paneId ?? soloPane.current;
  const isLocal = !source && !connection;
  // What this pane is attached to. Only a change here opens a new session.
  const sessionKey = source
    ? `stream:${JSON.stringify(source)}`
    : connection
      ? `ssh:${connection.key}`
      : shellId
        ? `local:${shellId}`
        : null;
  // The open callback reads the latest props through a ref, so the effect
  // depends on `sessionKey` alone and not on object identities.
  const target = useRef({ source, connection, shellId });
  target.current = { source, connection, shellId };
  const [scrolled, setScrolled] = useState(0);

  // §0.8: "Terminal font size... are user settings."
  const fontPx = useSettings((s) => s.terminalFontPx);
  const lineHeight = useSettings((s) => s.lineHeightPx)();
  const adjustFont = useSettings((s) => s.adjustFont);
  const resetFont = useSettings((s) => s.resetFont);
  // Painting happens inside callbacks and a rAF loop, so the current values are
  // mirrored into refs rather than captured in stale closures.
  const fontRef = useRef(fontPx);
  const lineRef = useRef(lineHeight);
  fontRef.current = fontPx;
  lineRef.current = lineHeight;

  /* ---- Measure the cell from the actual loaded font --------------------- */
  const measure = useCallback((ctx: CanvasRenderingContext2D): Cell => {
    // The native renderer's cells are whole pixels; the canvas must lay out
    // the same grid or the two disagree on the column count.
    if (gpuCell.current) return gpuCell.current;
    ctx.font = fontFor(termFamily(), fontRef.current, false, false);
    // "M" in a monospace face is the advance width. Measuring a long run and
    // dividing avoids accumulating the sub-pixel error of a single glyph.
    const w = ctx.measureText("M".repeat(64)).width / 64;
    return { w: w > 0 ? w : 8, h: lineRef.current };
  }, []);

  /* ---- Painting ---------------------------------------------------------- */

  const paintRow = useCallback((ctx: CanvasRenderingContext2D, line: LineUpdate) => {
    const { w, h } = cellRef.current;
    const y = PADDING + line.row * h;

    for (const span of line.spans) {
      const x = PADDING + span.start * w;
      ctx.fillStyle = span.bg;
      ctx.fillRect(x, y, span.len * w, h);

      const text = line.text.slice(span.start, span.start + span.len);
      if (text.trim() === "") continue;

      ctx.fillStyle = span.fg;
      ctx.font = fontFor(familyRef.current, fontRef.current, span.bold, span.italic);
      ctx.textBaseline = "middle";
      // Draw glyph by glyph at exact cell origins. A monospace face still
      // kerns and rounds sub-pixel advances across a long run, so drawing the
      // whole string at once lets characters drift away from their cells —
      // which is visible as text sliding out from under the cursor near the
      // right-hand side of a wide terminal.
      for (let i = 0; i < text.length; i++) {
        const ch = text[i] as string;
        if (ch !== " ") ctx.fillText(ch, x + i * w, y + h / 2);
      }

      if (span.underline) {
        ctx.fillRect(x, y + h - 2, span.len * w, 1);
      }
    }
  }, []);

  /** Repaint whichever row the cursor is leaving, then draw it at its new home. */
  const paintCursor = useCallback(
    (ctx: CanvasRenderingContext2D) => {
      const { w, h } = cellRef.current;
      const prev = paintedCursor.current;
      const { row, col } = cursor.current;

      // DECTCEM off (vim, htop, less while drawing): erase, draw nothing.
      if (!cursorVisible.current) {
        const stale = prev ? lines.current.get(prev.row) : undefined;
        if (stale) paintRow(ctx, stale);
        paintedCursor.current = null;
        return;
      }

      if (prev && (prev.row !== row || prev.col !== col)) {
        const stale = lines.current.get(prev.row);
        if (stale) paintRow(ctx, stale);
      }

      ctx.fillStyle = cursorColor.current;
      ctx.fillRect(PADDING + col * w, PADDING + row * h, Math.max(1, w), h);

      // Draw the character under the block in the background colour, so a
      // cursor parked on text reads as inverted rather than as a blob that ate
      // a letter.
      const line = lines.current.get(row);
      const ch = line?.text[col];
      if (ch && ch !== " ") {
        ctx.fillStyle = bgColor.current;
        ctx.font = fontFor(familyRef.current, fontRef.current, false, false);
        ctx.textBaseline = "middle";
        ctx.fillText(ch, PADDING + col * w, PADDING + row * h + h / 2);
      }

      paintedCursor.current = { row, col };
    },
    [paintRow],
  );

  const repaintAll = useCallback(
    (ctx: CanvasRenderingContext2D) => {
      const canvas = ctx.canvas;
      ctx.fillStyle = bgColor.current;
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      paintedCursor.current = null;
      for (const line of lines.current.values()) paintRow(ctx, line);
      paintCursor(ctx);
    },
    [paintCursor, paintRow],
  );

  /* ---- Selection --------------------------------------------------------- */

  const drawSelection = useCallback(() => {
    const ov = overlayRef.current;
    const ctx = ov?.getContext("2d");
    if (!ov || !ctx) return;
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, ov.width, ov.height);
    ctx.restore();

    const sel = selection.current;
    if (!sel) return;
    const [p, q] = ordered(sel);
    const { w, h } = cellRef.current;
    const { cols } = gridRef.current;
    ctx.fillStyle = selColor.current;
    ctx.globalAlpha = SELECTION_ALPHA;
    // O(selected rows): one rect per row.
    for (let r = p.row; r <= q.row; r++) {
      const c0 = r === p.row ? p.col : 0;
      const c1 = r === q.row ? q.col : cols - 1;
      ctx.fillRect(PADDING + c0 * w, PADDING + r * h, (c1 - c0 + 1) * w, h);
    }
    ctx.globalAlpha = 1;
  }, []);

  const clearSelection = useCallback(() => {
    // Always redraw, even when already empty: the highlight layer is the
    // source of truth only for what is drawn, and with the GPU path it can
    // sit hidden for minutes before an overlay reveals it.
    selection.current = null;
    drawSelection();
  }, [drawSelection]);

  /** The selected text, one line per row, trailing blanks trimmed. Rows are
   *  indexed by code point because the Rust side sends one per cell. */
  const selectedText = useCallback((): string => {
    const sel = selection.current;
    if (!sel) return "";
    const [p, q] = ordered(sel);
    const { cols } = gridRef.current;
    const out: string[] = [];
    for (let r = p.row; r <= q.row; r++) {
      const cells = Array.from(lines.current.get(r)?.text ?? "");
      const c0 = r === p.row ? p.col : 0;
      const c1 = r === q.row ? q.col : cols - 1;
      out.push(cells.slice(c0, c1 + 1).join("").trimEnd());
    }
    return out.join("\n");
  }, []);

  /** Linux convention: whatever is selected is on PRIMARY, for middle-click. */
  const publishPrimary = useCallback(() => {
    const text = selectedText();
    if (text) copyText(text, "primary").catch((e: unknown) => setError(String(e)));
  }, [selectedText]);

  const cellAt = useCallback((clientX: number, clientY: number): Point => {
    const box = wrapRef.current?.getBoundingClientRect();
    const { w, h } = cellRef.current;
    const { rows, cols } = gridRef.current;
    const x = clientX - (box?.left ?? 0) - PADDING;
    const y = clientY - (box?.top ?? 0) - PADDING;
    return {
      row: Math.min(rows - 1, Math.max(0, Math.floor(y / h))),
      col: Math.min(cols - 1, Math.max(0, Math.floor(x / w))),
    };
  }, []);

  /* ---- Open the session -------------------------------------------------- */

  const fitToBox = useCallback((): { rows: number; cols: number } => {
    const box = wrapRef.current?.getBoundingClientRect();
    const { w, h } = cellRef.current;
    if (!box || box.width < 1) return gridRef.current;
    return {
      rows: Math.max(1, Math.floor((box.height - PADDING * 2) / h)),
      cols: Math.max(1, Math.floor((box.width - PADDING * 2) / w)),
    };
  }, []);

  /* ---- Discover shells once; picking one opens the session --------------- */
  useEffect(() => {
    // Remote and stream panes have no shell picker. Discovering shells there
    // used to set `shellId`, re-run the open effect, and open a SECOND SSH
    // channel / container exec while the first was still in flight.
    if (!isLocal) return;
    let cancelled = false;
    listShells()
      .then((list) => {
        if (cancelled) return;
        setShells(list);
        setShellId((current) => current ?? list[0]?.id ?? null);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [isLocal]);

  useEffect(() => {
    let cancelled = false;
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;

    (async () => {
      // Without this the first measurement is of the fallback face and every
      // column is wrong — the same failure mode UI_BUILD_GUIDE §0.9 calls out.
      await document.fonts.ready;
      if (cancelled) return;

      familyRef.current = termFamily();
      cursorColor.current = cssVar("--amber-500");
      selColor.current = cssVar("--amber-500");
      bgColor.current = cssVar("--chassis-900");
      cellRef.current = measure(ctx);
      const { rows, cols } = fitToBox();
      gridRef.current = { rows, cols };

      // §0.8 is keyboard-first, and a terminal you have to click before it
      // accepts a key is broken on arrival.
      wrapRef.current?.focus();

      if (!sessionKey) return; // local pane still waiting for the shell list

      try {
        const id = await acquire(pane, sessionKey, () => {
          const t = target.current;
          if (t.source) {
            // Exec or log follow: the Rust side owns the stream; to this
            // component it is a session id like any other.
            return openStream(t.source, rows, cols);
          }
          if (t.connection) {
            // A remote shell is a channel on the pooled transport (§8.1), not
            // a new handshake.
            return sshOpenShell(t.connection.key, rows, cols);
          }
          return openSession(rows, cols, t.shellId ?? undefined);
        });
        // Unmounted while waiting: the session belongs to the pane and stays
        // open; the next mount attaches to it.
        if (cancelled) return;
        sessionId.current = id;
        generation.current = 0;
        lines.current.clear();
        setExited(false);
        setError(null);
        // Re-fit now that the session exists: the box may have changed during
        // the open, and an attach to an existing session needs the PTY told
        // our size. A resize also makes the next snapshot a full repaint,
        // which is exactly what a freshly mounted canvas needs.
        const now = fitToBox();
        gridRef.current = now;
        resizeSession(id, now.rows, now.cols).catch(() => undefined);
      } catch (e) {
        if (!cancelled && String(e) !== "Error: pane closed") setError(String(e));
      }
    })();

    return () => {
      cancelled = true;
      // NOT closed: unmounting is not the user ending the session (tab switch,
      // module switch, split). Panes without an id own theirs, though.
      sessionId.current = null;
      if (!paneId) release(pane);
    };
  }, [fitToBox, measure, sessionKey, pane, paneId]);

  /* ---- Poll: one snapshot per frame, never one message per write --------- */

  useEffect(() => {
    let raf = 0;
    let inFlight = false;
    // A poll was asked for while one was in flight: run it as soon as that one
    // lands, or the keystroke waits for the next frame after all.
    let again = false;

    const poll = () => {
      const id = sessionId.current;
      const canvas = canvasRef.current;
      const ctx = canvas?.getContext("2d");
      if (id === null || !ctx) return;
      if (inFlight) {
        again = true;
        return;
      }

      inFlight = true;
      snapshotSession(id, generation.current)
        .then((snap) => {
          inFlight = false;
          if (again) {
            again = false;
            queueMicrotask(poll);
          }
          // The pane switched sessions (shell picker) while this was in
          // flight: the snapshot belongs to the old one.
          if (id !== sessionId.current) return;
          if (!snap) return; // nothing changed — the common case
          generation.current = snap.generation;
          cursor.current = { row: snap.cursor_row, col: snap.cursor_col };
          modes.current = { appCursor: snap.app_cursor };
          cursorVisible.current = snap.cursor_visible;
          gridRef.current = { rows: snap.rows, cols: snap.cols };
          if (!snap.alive) setExited(true);
          setScrolled(snap.scroll_offset);

          // The native view is showing: keep the rows (selection text, word
          // select, the canvas fallback) but draw nothing.
          const paints = !(gpu.current.live && !gpu.current.covered);

          if (snap.full) {
            // Scrolled or resized: viewport rows now hold different lines, so a
            // selection in viewport coordinates would point at the wrong text.
            clearSelection();
            lines.current.clear();
            for (const l of snap.lines) lines.current.set(l.row, l);
            if (paints) repaintAll(ctx);
            return;
          }
          if (!paints) {
            for (const l of snap.lines) lines.current.set(l.row, l);
            return;
          }

          // Damage-driven: repaint only the rows Rust says changed, plus the two
          // rows the cursor touched.
          for (const l of snap.lines) {
            lines.current.set(l.row, l);
            paintRow(ctx, l);
          }
          const cur = lines.current.get(cursor.current.row);
          if (cur && !snap.lines.some((l) => l.row === cursor.current.row)) paintRow(ctx, cur);
          paintCursor(ctx);
        })
        .catch((e: unknown) => {
          inFlight = false;
          again = false;
          setError(String(e));
        });
    };

    /** Report this pane to the native renderer when anything it draws from
     *  changes. One `getBoundingClientRect` and a string compare per frame. */
    const syncGpu = () => {
      const id = sessionId.current;
      const wrap = wrapRef.current;
      const ctx = canvasRef.current?.getContext("2d");
      if (id === null || !wrap || !ctx || !gpuAvailable()) return;
      const g = gpu.current;
      const r = wrap.getBoundingClientRect();
      const covered = r.width < 1 || r.height < 1 || coveredByOverlay(r);
      if (covered !== g.covered) {
        g.covered = covered;
        // The overlay is up this frame; the native view hides one IPC later.
        // Paint the canvas now so there is something under the overlay.
        if (covered && g.live) {
          repaintAll(ctx);
          drawSelection();
        }
      }
      const sel = selection.current ? ordered(selection.current) : null;
      const state: PaneState = {
        pane,
        session: id,
        rect: { x: r.left, y: r.top, w: r.width, h: r.height },
        dpr: window.devicePixelRatio || 1,
        font_px: fontRef.current,
        line_px: lineRef.current,
        visible: !covered,
        focused: document.activeElement === wrap,
        selection: sel ? [[sel[0].row, sel[0].col], [sel[1].row, sel[1].col]] : null,
        targets: (() => {
          const { broadcast: on, tabId: tab } = bcast.current;
          const tree = on && tab ? usePanes.getState().trees[tab] : undefined;
          return tree ? sessionsFor(leaves(tree)) : [id];
        })(),
      };
      const key = JSON.stringify(state);
      // Unchanged: nothing to send, unless still waiting for the view to go live.
      if (key === g.sent && (g.live || covered)) return;
      if (g.inFlight) {
        // A focus change must not wait a frame behind an in-flight update:
        // keys typed meanwhile would go to the pane focus just left.
        g.again = true;
        return;
      }
      g.inFlight = true;
      g.sent = key;
      const settle = () => {
        g.inFlight = false;
        if (g.again) {
          g.again = false;
          syncGpu();
        }
      };
      gpuPaneSet(state)
        .then((info) => {
          settle();
          if (!info) {
            // No GPU path here: back to the canvas, measured by the canvas.
            const was = g.live;
            g.live = false;
            if (gpuCell.current) {
              gpuCell.current = null;
              cellRef.current = measure(ctx);
            }
            if (was) repaintAll(ctx);
            return;
          }
          const cell = gpuCell.current;
          if (!cell || cell.w !== info.cell_w || cell.h !== info.cell_h) {
            gpuCell.current = { w: info.cell_w, h: info.cell_h };
            cellRef.current = gpuCell.current;
            const fit = fitToBox();
            const sid = sessionId.current;
            if (sid !== null && (fit.rows !== gridRef.current.rows || fit.cols !== gridRef.current.cols)) {
              gridRef.current = fit;
              quiet(resizeSession(sid, fit.rows, fit.cols));
            }
            if (!g.live || g.covered) repaintAll(ctx);
          }
          g.live = info.live;
        })
        .catch(settle);
    };

    const tick = () => {
      raf = requestAnimationFrame(tick);
      poll();
      syncGpu();
    };

    pollNow.current = poll;
    syncNow.current = syncGpu;
    raf = requestAnimationFrame(tick);
    return () => {
      pollNow.current = () => {};
      syncNow.current = () => {};
      cancelAnimationFrame(raf);
    };
  }, [clearSelection, drawSelection, paintCursor, paintRow, repaintAll, fitToBox, measure, pane]);

  // The native view belongs to this mount. A remount (tab switch, split)
  // attaches a new one; the session itself lives on (§18.18).
  useEffect(
    () => () => {
      gpuPaneRemove(pane);
      gpu.current = { live: false, covered: false, inFlight: false, again: false, sent: "" };
    },
    [pane],
  );

  // Rust typed a key here natively: clear the selection, as typing always has.
  useEffect(() => {
    let off: (() => void) | null = null;
    let cancelled = false;
    listen<string>(NATIVE_KEY_EVENT, (e) => {
      if (e.payload === pane) clearSelection();
    })
      .then((f) => {
        if (cancelled) f();
        else off = f;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      off?.();
    };
  }, [pane, clearSelection]);

  /* ---- Resize ------------------------------------------------------------ */

  useEffect(() => {
    const wrap = wrapRef.current;
    const canvas = canvasRef.current;
    if (!wrap || !canvas) return;

    const ro = new ResizeObserver(() => {
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      const box = wrap.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;

      // Back the canvas at device resolution and scale the context, or the text
      // is soft at 125%/150% Windows scaling — the same hazard §0.9 flags for
      // hairlines.
      canvas.width = Math.max(1, Math.floor(box.width * dpr));
      canvas.height = Math.max(1, Math.floor(box.height * dpr));
      canvas.style.width = `${box.width}px`;
      canvas.style.height = `${box.height}px`;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

      const ov = overlayRef.current;
      const octx = ov?.getContext("2d");
      if (ov && octx) {
        ov.width = canvas.width;
        ov.height = canvas.height;
        octx.setTransform(dpr, 0, 0, dpr, 0, 0);
      }
      // Cell geometry may have changed under the selection.
      selection.current = null;

      familyRef.current = termFamily();
      cellRef.current = measure(ctx);
      const { rows, cols } = fitToBox();
      const id = sessionId.current;
      if (id !== null && (rows !== gridRef.current.rows || cols !== gridRef.current.cols)) {
        gridRef.current = { rows, cols };
        quiet(resizeSession(id, rows, cols));
      }
      repaintAll(ctx);
    });

    ro.observe(wrap);
    return () => ro.disconnect();
  }, [fitToBox, measure, repaintAll]);

  /* ---- Font size changes re-measure and resize the session --------------- */

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!ctx) return;

    cellRef.current = measure(ctx);
    clearSelection();
    const { rows, cols } = fitToBox();
    const id = sessionId.current;
    if (id !== null) {
      gridRef.current = { rows, cols };
      // The shell must be told, or it keeps wrapping at the old width and the
      // output no longer matches the pane.
      quiet(resizeSession(id, rows, cols));
    }
    repaintAll(ctx);
  }, [fontPx, lineHeight, fitToBox, measure, repaintAll, clearSelection]);

  /* ---- Input ------------------------------------------------------------- */

  /** Mouse wheel scrolls history. Three lines per notch, the platform default. */
  const onWheel = useCallback((e: React.WheelEvent<HTMLDivElement>) => {
    const id = sessionId.current;
    if (id === null) return;
    // deltaY > 0 is "toward the user" = forward in time = out of history.
    const lines = e.deltaMode === 1 ? e.deltaY : e.deltaY / 40;
    const delta = Math.round(-lines * 3);
    if (delta !== 0) quiet(scrollSession(id, delta));
  }, []);

  /** Where keystrokes and pastes go: this pane, or every pane in the tab while
   *  broadcasting (Step 9). Never "every open session" — a background tab must
   *  not be typed into. */
  const targets = useCallback((): number[] => {
    const id = sessionId.current;
    if (id === null) return [];
    if (broadcast && tabId) {
      const tree = usePanes.getState().trees[tabId];
      return tree ? sessionsFor(leaves(tree)) : [id];
    }
    return [id];
  }, [broadcast, tabId]);

  const paste = useCallback(
    (from: ClipboardSelection) => {
      clearSelection();
      pasteInto(targets(), from).catch((e: unknown) => setError(String(e)));
    },
    [clearSelection, targets],
  );

  const onKeyDown = useCallback((e: React.KeyboardEvent<HTMLDivElement>) => {
    const id = sessionId.current;
    if (id === null) return;
    const ev = e.nativeEvent;
    const claim = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    // Font zoom is not handled here: AppShell claims it in the capture phase
    // for the whole window. Handling it here too changed the size twice.

    // Copy: Ctrl+Shift+C / ⌘C, and Ctrl+Insert. With nothing selected it does
    // nothing — in particular it must not fall through and send Ctrl+C.
    if (isAppChord(ev, CHORD.copy) || (e.ctrlKey && !e.shiftKey && e.key === "Insert")) {
      const text = selectedText();
      if (text) copyText(text, "clipboard").catch((err: unknown) => setError(String(err)));
      claim();
      return;
    }
    // Paste: Ctrl+Shift+V / ⌘V, and Shift+Insert.
    if (isAppChord(ev, CHORD.paste) || (e.shiftKey && !e.ctrlKey && e.key === "Insert")) {
      paste("clipboard");
      claim();
      return;
    }

    // §0.8 wants every action reachable without a mouse, and scrollback is an
    // action. Shift+PageUp/Down is the convention every terminal uses.
    if (e.shiftKey && !e.ctrlKey && (e.key === "PageUp" || e.key === "PageDown")) {
      const page = Math.max(1, gridRef.current.rows - 1);
      quiet(scrollSession(id, e.key === "PageUp" ? page : -page));
      claim();
      return;
    }

    const bytes = keyToBytes(ev, modes.current);
    if (!bytes) return;
    claim();
    clearSelection();
    for (const target of targets()) {
      // Repaint as soon as the write lands: the prediction is in the next
      // snapshot (§5.4 input path).
      quiet(writeSession(target, bytes).then(() => pollNow.current()));
    }
  }, [clearSelection, paste, selectedText, targets]);

  /* ---- Mouse: select, copy to PRIMARY, middle-click paste ---------------- */

  const onMouseDown = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      // Middle button: paste PRIMARY, as every X11 and Wayland terminal does.
      if (e.button === 1) {
        e.preventDefault();
        wrapRef.current?.focus();
        paste("primary");
        return;
      }
      if (e.button !== 0) return;
      // preventDefault stops the browser starting a native text drag, and also
      // its focus handling — so focus explicitly.
      e.preventDefault();
      wrapRef.current?.focus();
      endDrag.current?.();

      const p = cellAt(e.clientX, e.clientY);

      // Double-click: word. Triple-click: whole row.
      if (e.detail >= 2) {
        const cells = Array.from(lines.current.get(p.row)?.text ?? "");
        const [c0, c1] =
          e.detail === 2 ? wordAt(cells, p.col) : [0, gridRef.current.cols - 1];
        selection.current = { a: { row: p.row, col: c0 }, b: { row: p.row, col: c1 } };
        drawSelection();
        publishPrimary();
        return;
      }

      // Shift+click extends the existing selection to here.
      const existing = selection.current;
      if (e.shiftKey && existing) {
        selection.current = { a: existing.a, b: p };
        drawSelection();
        publishPrimary();
        return;
      }

      // A plain click clears; only an actual drag selects, so clicking to focus
      // never highlights the cell under the pointer.
      clearSelection();
      const anchor = p;
      const move = (ev: MouseEvent) => {
        const q = cellAt(ev.clientX, ev.clientY);
        if (!selection.current && q.row === anchor.row && q.col === anchor.col) return;
        selection.current = { a: anchor, b: q };
        drawSelection();
      };
      const stop = () => {
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        endDrag.current = null;
      };
      const up = () => {
        stop();
        publishPrimary();
      };
      // On window, so a drag that leaves the pane keeps tracking.
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
      endDrag.current = stop;
    },
    [cellAt, clearSelection, drawSelection, paste, publishPrimary],
  );

  // A pane closed mid-drag must not leave listeners on the window.
  useEffect(() => () => endDrag.current?.(), []);

  return (
    <section
      data-testid="session-surface"
      className="relative flex min-w-0 flex-1 flex-col overflow-hidden bg-chassis-900"
    >
      <div className="flex h-tabstrip shrink-0 items-center gap-2 border-b-hairline border-bezel-500 bg-chassis-800 px-2">
        {source ? (
          <>
            <span aria-hidden className="h-[6px] w-[6px] rounded-full bg-amber-500" />
            <span className="num text-micro text-text-hi">{streamTitle(source)}</span>
            {isReadOnly(source) && (
              <span className="font-condensed uppercase text-eyebrow text-text-lo">read-only · follow</span>
            )}
          </>
        ) : connection ? (
          <>
            {/* Cyan is link state, and a live SSH session is exactly that (§0.2). */}
            <span aria-hidden className="h-[6px] w-[6px] rounded-full bg-cyan-400" />
            <span className="num text-micro text-text-hi">
              {connection.user}@{connection.host}
            </span>
            <span className="num text-micro text-text-lo">· {connection.identity}</span>
            <button
              type="button"
              onClick={() => onDisconnect?.()}
              className="num rounded border-hairline border-bezel-500 px-[5px] text-micro
                         text-text-mid hover:border-red-500 hover:text-red-500"
            >
              disconnect
            </button>
          </>
        ) : (
          <>
            <span className="font-condensed uppercase text-eyebrow text-text-lo">shell</span>
            <select
              aria-label="Shell"
              value={shellId ?? ""}
              onChange={(e) => setShellId(e.target.value)}
              className="num rounded border-hairline border-bezel-500 bg-transparent px-1 py-[1px]
                         text-micro text-text-mid focus:border-amber-500"
            >
              {shells.map((sh) => (
                <option key={sh.id} value={sh.id}>
                  {sh.label}
                </option>
              ))}
            </select>
            <button
              type="button"
              onClick={() => onConnect?.()}
              className="num rounded border-hairline border-bezel-500 px-[5px] text-micro
                         text-text-mid hover:border-amber-500 hover:text-amber-500"
            >
              ⇄ ssh…
            </button>
          </>
        )}

        <span className="ml-auto flex items-center gap-1">
          <span className="font-condensed uppercase text-eyebrow text-text-lo">size</span>
          <button
            type="button"
            aria-label="Smaller text"
            onClick={() => adjustFont(-1)}
            className="num rounded border-hairline border-bezel-500 px-[5px] text-micro
                       text-text-mid hover:border-amber-500 hover:text-amber-500"
          >
            −
          </button>
          <button
            type="button"
            title="Reset text size"
            onClick={resetFont}
            className="num w-10 text-center text-micro text-text-lo hover:text-amber-500"
          >
            {fontPx}
          </button>
          <button
            type="button"
            aria-label="Bigger text"
            onClick={() => adjustFont(1)}
            className="num rounded border-hairline border-bezel-500 px-[5px] text-micro
                       text-text-mid hover:border-amber-500 hover:text-amber-500"
          >
            +
          </button>
        </span>

        {scrolled > 0 && (
          <button
            type="button"
            onClick={() => {
              const id = sessionId.current;
              if (id !== null) quiet(scrollSession(id, -1_000_000));
            }}
            className="num rounded border-hairline border-amber-500 bg-amber-900 px-[5px]
                       text-micro text-amber-500"
          >
            ↑ {scrolled} lines back · jump to bottom
          </button>
        )}
      </div>

      <div
        ref={wrapRef}
        tabIndex={0}
        role="application"
        aria-label="Terminal"
        onKeyDown={onKeyDown}
        onFocus={() => syncNow.current()}
        onBlur={() => syncNow.current()}
        onWheel={onWheel}
        onMouseDown={onMouseDown}
        className="term relative min-h-0 flex-1 cursor-text outline-none focus-visible:outline-none"
      >
        <canvas ref={canvasRef} className="block h-full w-full" />
        <canvas
          ref={overlayRef}
          aria-hidden
          className="pointer-events-none absolute inset-0 block h-full w-full"
        />
        {!active && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
            <span className="text-small text-text-lo">click to focus</span>
          </div>
        )}
      </div>

      {(error || exited) && (
        <div className="num border-t-hairline border-bezel-500 bg-chassis-800 px-3 py-1 text-micro text-red-500">
          {error ?? "session ended"}
        </div>
      )}
    </section>
  );
}
