import { useCallback, useEffect, useRef, useState } from "react";
import { keyToBytes } from "@/lib/keys";
import {
  closeSession,
  listShells,
  openSession,
  resizeSession,
  scrollSession,
  snapshotSession,
  writeSession,
  type LineUpdate,
  type ShellInfo,
} from "@/lib/session";
import { sshOpenShell, type ConnectionInfo } from "@/lib/remote";
import { registerPane, sessionsFor, unregisterPane } from "@/lib/broadcast";
import { leaves, usePanes } from "@/state/panes";
import { isFontZoomKey, useSettings } from "@/state/settings";

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
 */

const PADDING = 6;

type Cell = { w: number; h: number };

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
  onConnect,
  onDisconnect,
  paneId,
  tabId,
  broadcast = false,
}: {
  active: boolean;
  /** When set, the session is a shell on this host instead of a local one. */
  connection?: ConnectionInfo | null;
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

  const sessionId = useRef<number | null>(null);
  const generation = useRef(0);
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

  const [error, setError] = useState<string | null>(null);
  const [exited, setExited] = useState(false);
  const [shells, setShells] = useState<ShellInfo[]>([]);
  const [shellId, setShellId] = useState<string | null>(null);
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
  }, []);

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
      bgColor.current = cssVar("--chassis-900");
      cellRef.current = measure(ctx);
      const { rows, cols } = fitToBox();
      gridRef.current = { rows, cols };

      // §0.8 is keyboard-first, and a terminal you have to click before it
      // accepts a key is broken on arrival.
      wrapRef.current?.focus();

      try {
        generation.current = 0;
        lines.current.clear();
        if (connection) {
          // A remote shell is a channel on the pooled transport (§8.1), not a
          // new handshake — and to everything above this line it is just a
          // session id like any other.
          sessionId.current = await sshOpenShell(connection.key, rows, cols);
        } else {
          // Only open once a shell has been chosen. Discovery lives in its own
          // effect; doing it here meant this effect set `shellId`, which is one
          // of its own dependencies, so it re-ran and opened a SECOND session.
          if (!shellId) return;
          sessionId.current = await openSession(rows, cols, shellId);
        }
        if (paneId && sessionId.current !== null) {
          registerPane(paneId, sessionId.current);
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();

    return () => {
      cancelled = true;
      const id = sessionId.current;
      sessionId.current = null;
      if (paneId) unregisterPane(paneId);
      if (id !== null) void closeSession(id);
    };
  }, [fitToBox, measure, shellId, connection, paneId]);

  /* ---- Poll: one snapshot per frame, never one message per write --------- */

  useEffect(() => {
    let raf = 0;
    let inFlight = false;

    const tick = () => {
      raf = requestAnimationFrame(tick);
      const id = sessionId.current;
      const canvas = canvasRef.current;
      const ctx = canvas?.getContext("2d");
      if (id === null || !ctx || inFlight) return;

      inFlight = true;
      snapshotSession(id, generation.current)
        .then((snap) => {
          inFlight = false;
          if (!snap) return; // nothing changed — the common case
          generation.current = snap.generation;
          cursor.current = { row: snap.cursor_row, col: snap.cursor_col };
          gridRef.current = { rows: snap.rows, cols: snap.cols };
          if (!snap.alive) setExited(true);
          setScrolled(snap.scroll_offset);

          if (snap.full) {
            lines.current.clear();
            for (const l of snap.lines) lines.current.set(l.row, l);
            repaintAll(ctx);
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
          setError(String(e));
        });
    };

    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [paintCursor, paintRow, repaintAll]);

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

      familyRef.current = termFamily();
      cellRef.current = measure(ctx);
      const { rows, cols } = fitToBox();
      const id = sessionId.current;
      if (id !== null && (rows !== gridRef.current.rows || cols !== gridRef.current.cols)) {
        gridRef.current = { rows, cols };
        void resizeSession(id, rows, cols);
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
    const { rows, cols } = fitToBox();
    const id = sessionId.current;
    if (id !== null) {
      gridRef.current = { rows, cols };
      // The shell must be told, or it keeps wrapping at the old width and the
      // output no longer matches the pane.
      void resizeSession(id, rows, cols);
    }
    repaintAll(ctx);
  }, [fontPx, lineHeight, fitToBox, measure, repaintAll]);

  /* ---- Input ------------------------------------------------------------- */

  /** Mouse wheel scrolls history. Three lines per notch, the platform default. */
  const onWheel = useCallback((e: React.WheelEvent<HTMLDivElement>) => {
    const id = sessionId.current;
    if (id === null) return;
    // deltaY > 0 is "toward the user" = forward in time = out of history.
    const lines = e.deltaMode === 1 ? e.deltaY : e.deltaY / 40;
    const delta = Math.round(-lines * 3);
    if (delta !== 0) void scrollSession(id, delta);
  }, []);

  const onKeyDown = useCallback((e: React.KeyboardEvent<HTMLDivElement>) => {
    const id = sessionId.current;
    if (id === null) return;
    // Font zoom first: Ctrl +/-/0 must never reach the shell as input.
    const zoom = isFontZoomKey(e.nativeEvent);
    if (zoom) {
      if (zoom === "in") adjustFont(1);
      else if (zoom === "out") adjustFont(-1);
      else resetFont();
      e.preventDefault();
      e.stopPropagation();
      return;
    }

    // §0.8 wants every action reachable without a mouse, and scrollback is an
    // action. Shift+PageUp/Down is the convention every terminal uses.
    if (e.shiftKey && (e.key === "PageUp" || e.key === "PageDown")) {
      const page = Math.max(1, gridRef.current.rows - 1);
      void scrollSession(id, e.key === "PageUp" ? page : -page);
      e.preventDefault();
      e.stopPropagation();
      return;
    }

    const bytes = keyToBytes(e.nativeEvent);
    if (!bytes) return;
    e.preventDefault();
    e.stopPropagation();

    if (broadcast && tabId) {
      // Step 9's broadcast. The target list comes from THIS tab's pane tree,
      // never "every open session" — a background tab must not be typed into.
      const tree = usePanes.getState().trees[tabId];
      const targets = tree ? sessionsFor(leaves(tree)) : [id];
      for (const target of targets) void writeSession(target, bytes);
      return;
    }

    void writeSession(id, bytes);
  }, [adjustFont, resetFont, broadcast, tabId]);

  return (
    <section
      data-testid="session-surface"
      className="relative flex min-w-0 flex-1 flex-col overflow-hidden bg-chassis-900"
    >
      <div className="flex h-tabstrip shrink-0 items-center gap-2 border-b-hairline border-bezel-500 bg-chassis-800 px-2">
        {connection ? (
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
              if (id !== null) void scrollSession(id, -1_000_000);
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
        onWheel={onWheel}
        className="term relative min-h-0 flex-1 outline-none focus-visible:outline-none"
      >
        <canvas ref={canvasRef} className="block h-full w-full" />
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
