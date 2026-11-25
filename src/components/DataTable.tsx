import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

/**
 * The shared virtualised table — UI_BUILD_GUIDE Step 12. Docker containers,
 * Kubernetes resources and the database result grid are all this.
 *
 * Built for the largest of them, the 1 M-row SQL grid (CLAUDE.md §4: 60 fps
 * scroll):
 *
 *   · **Rows are pulled, not pushed.** The table asks `rowAt(i)` for the rows it
 *     is about to mount and nothing else, so a caller can keep a million rows in
 *     Rust and ship a window of ~50 over IPC. `undefined` renders as a loading
 *     row, and `onVisibleRange` tells the caller what to fetch next.
 *   · **Only visible rows mount** (@tanstack/react-virtual), and rows are keyed,
 *     so a list that changes underneath — a pod restarting — patches one row
 *     instead of rebuilding the table (§10 "keyed reconciliation").
 *   · **Sorting is the caller's.** The header only reports the requested order.
 *     The Docker list sorts a few hundred rows in JS; the SQL grid sorts in Rust
 *     over Arrow columns. Neither belongs in a render path.
 *
 * Fixed pixel widths, resizable from the header edge, and a horizontal scroll
 * when they exceed the pane: a 40-column query result must stay readable rather
 * than squeezing every column to an ellipsis.
 */

export interface Column<R> {
  id: string;
  header: string;
  /** Initial width in px. */
  width: number;
  align?: "left" | "right";
  /** Rich cell. Defaults to `text(row)`. */
  render?: (row: R) => ReactNode;
  /** Plain text: the tooltip, and the default cell. */
  text: (row: R) => string;
  sortable?: boolean;
}

export type SortDir = "asc" | "desc";
export interface Sort {
  column: string;
  dir: SortDir;
}

export const ROW_H = 24;
const HEADER_H = 26;
const MIN_COL = 40;
const OVERSCAN = 12;

export function DataTable<R>({
  columns,
  rowCount,
  rowAt,
  rowKey,
  onVisibleRange,
  sort,
  onSort,
  selectedKey,
  onSelect,
  onActivate,
  empty = "nothing to show",
  label,
}: {
  columns: Column<R>[];
  rowCount: number;
  rowAt: (index: number) => R | undefined;
  rowKey: (row: R, index: number) => string;
  /** First and last index the table is about to render (overscan included). */
  onVisibleRange?: (start: number, end: number) => void;
  sort?: Sort | null;
  onSort?: (sort: Sort | null) => void;
  selectedKey?: string | null;
  onSelect?: (row: R, index: number) => void;
  /** Enter or double-click. */
  onActivate?: (row: R, index: number) => void;
  empty?: string;
  label: string;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);

  // Widths are local state seeded from the column definitions; a resize is a
  // view preference, not data. Keyed by column id so reordering keeps widths.
  const [widths, setWidths] = useState<Record<string, number>>({});
  const widthOf = useCallback(
    (c: Column<R>) => widths[c.id] ?? c.width,
    [widths],
  );
  const template = useMemo(
    () => columns.map((c) => `${widthOf(c)}px`).join(" "),
    [columns, widthOf],
  );
  const totalWidth = useMemo(
    () => columns.reduce((sum, c) => sum + widthOf(c), 0),
    [columns, widthOf],
  );

  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: OVERSCAN,
  });
  const items = virtualizer.getVirtualItems();
  const first = items[0]?.index ?? 0;
  const last = items[items.length - 1]?.index ?? -1;

  useEffect(() => {
    if (onVisibleRange && last >= first) onVisibleRange(first, last);
  }, [first, last, onVisibleRange]);

  /* ---- Keyboard: the selection moves, the view follows ------------------- */

  const selectedIndex = useMemo(() => {
    if (selectedKey == null) return -1;
    // Only the mounted window is searched: O(visible), never O(rowCount).
    for (const it of items) {
      const r = rowAt(it.index);
      if (r !== undefined && rowKey(r, it.index) === selectedKey) return it.index;
    }
    return -1;
  }, [items, rowAt, rowKey, selectedKey]);

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (rowCount === 0) return;
      const page = Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 240) / ROW_H) - 1);
      const from = selectedIndex < 0 ? first : selectedIndex;
      let next = from;
      switch (e.key) {
        case "ArrowDown":
          next = from + 1;
          break;
        case "ArrowUp":
          next = from - 1;
          break;
        case "PageDown":
          next = from + page;
          break;
        case "PageUp":
          next = from - page;
          break;
        case "Home":
          next = 0;
          break;
        case "End":
          next = rowCount - 1;
          break;
        case "Enter": {
          const r = selectedIndex >= 0 ? rowAt(selectedIndex) : undefined;
          if (r !== undefined) onActivate?.(r, selectedIndex);
          e.preventDefault();
          return;
        }
        default:
          return;
      }
      next = Math.min(rowCount - 1, Math.max(0, next));
      virtualizer.scrollToIndex(next, { align: "auto" });
      const r = rowAt(next);
      if (r !== undefined) onSelect?.(r, next);
      e.preventDefault();
    },
    [first, onActivate, onSelect, rowAt, rowCount, selectedIndex, virtualizer],
  );

  /* ---- Column resize ------------------------------------------------------ */

  /** Removes the listeners of a resize in progress; also run on unmount. */
  const endResize = useRef<(() => void) | null>(null);
  /** A resize just ended: swallow the click it produces on the header, or
   *  every column resize would also toggle the sort (in the SQL grid, a full
   *  load-and-sort of the result). */
  const justResized = useRef(false);
  useEffect(() => () => endResize.current?.(), []);

  const startResize = (c: Column<R>, e: React.PointerEvent<HTMLSpanElement>) => {
    e.preventDefault();
    e.stopPropagation();
    endResize.current?.();
    const startX = e.clientX;
    const startW = widthOf(c);
    const move = (ev: PointerEvent) => {
      const w = Math.max(MIN_COL, Math.round(startW + ev.clientX - startX));
      setWidths((prev) => ({ ...prev, [c.id]: w }));
    };
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", stop);
      endResize.current = null;
    };
    const up = () => {
      stop();
      justResized.current = true;
      window.setTimeout(() => {
        justResized.current = false;
      }, 0);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    // Alt-Tab or a touch interruption mid-drag: no pointerup ever comes.
    window.addEventListener("pointercancel", stop);
    endResize.current = stop;
  };

  const cycleSort = (c: Column<R>) => {
    if (justResized.current) return;
    if (!onSort || c.sortable === false) return;
    // asc -> desc -> unsorted, the cycle every grid uses.
    if (!sort || sort.column !== c.id) onSort({ column: c.id, dir: "asc" });
    else if (sort.dir === "asc") onSort({ column: c.id, dir: "desc" });
    else onSort(null);
  };

  return (
    <div
      ref={scrollRef}
      role="grid"
      aria-label={label}
      aria-rowcount={rowCount}
      tabIndex={0}
      onKeyDown={onKeyDown}
      className="relative min-h-0 flex-1 overflow-auto outline-none focus-visible:outline-none"
    >
      <div style={{ width: totalWidth, minWidth: "100%" }}>
        {/* Sticky header: stays put while rows scroll under it. */}
        <div
          role="row"
          className="sticky top-0 z-10 grid border-b-hairline border-bezel-500 bg-chassis-800"
          style={{ gridTemplateColumns: template, height: HEADER_H }}
        >
          {columns.map((c) => {
            const active = sort?.column === c.id;
            return (
              <div
                key={c.id}
                role="columnheader"
                aria-sort={active ? (sort?.dir === "asc" ? "ascending" : "descending") : "none"}
                onClick={() => cycleSort(c)}
                className={`relative flex items-center gap-1 overflow-hidden px-2 font-condensed
                            uppercase text-eyebrow ${active ? "text-amber-500" : "text-text-lo"}
                            ${onSort && c.sortable !== false ? "cursor-pointer hover:text-text-hi" : ""}
                            ${c.align === "right" ? "justify-end" : ""}`}
                title={c.header}
              >
                <span className="truncate">{c.header}</span>
                {active && <span aria-hidden>{sort?.dir === "asc" ? "▲" : "▼"}</span>}
                <span
                  aria-hidden
                  onPointerDown={(e) => startResize(c, e)}
                  onClick={(e) => e.stopPropagation()}
                  className="absolute right-0 top-0 h-full w-[6px] cursor-col-resize hover:bg-bezel-500"
                />
              </div>
            );
          })}
        </div>

        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {items.map((it) => {
            const row = rowAt(it.index);
            const key = row !== undefined ? rowKey(row, it.index) : `loading-${it.index}`;
            const selected = row !== undefined && key === selectedKey;
            return (
              <div
                key={key}
                role="row"
                aria-rowindex={it.index + 1}
                aria-selected={selected}
                onClick={() => row !== undefined && onSelect?.(row, it.index)}
                onDoubleClick={() => row !== undefined && onActivate?.(row, it.index)}
                className={`row-hoverable absolute left-0 grid w-full cursor-default items-center
                            text-small ${selected ? "bg-chassis-700 text-text-hi" : "text-text-mid"}`}
                style={{
                  gridTemplateColumns: template,
                  height: ROW_H,
                  transform: `translateY(${it.start}px)`,
                }}
              >
                {row === undefined ? (
                  <span className="px-2 text-micro text-text-lo">…</span>
                ) : (
                  columns.map((c) => (
                    <span
                      key={c.id}
                      role="gridcell"
                      title={c.text(row)}
                      className={`truncate px-2 ${c.align === "right" ? "num text-right" : ""}`}
                    >
                      {c.render ? c.render(row) : c.text(row)}
                    </span>
                  ))
                )}
              </div>
            );
          })}
        </div>

        {rowCount === 0 && <p className="p-3 text-small text-text-lo">{empty}</p>}
      </div>
    </div>
  );
}

/** Sort a small in-memory list by a column's text, numbers numerically. For
 *  lists of hundreds (containers, pods); the SQL grid sorts in Rust instead. */
export function sortRows<R>(rows: readonly R[], columns: Column<R>[], sort: Sort | null): R[] {
  if (!sort) return rows as R[];
  const col = columns.find((c) => c.id === sort.column);
  if (!col) return rows as R[];
  const coll = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
  const keyed = rows.map((r) => [col.text(r), r] as const);
  keyed.sort((a, b) => coll.compare(a[0], b[0]));
  if (sort.dir === "desc") keyed.reverse();
  return keyed.map(([, r]) => r);
}
