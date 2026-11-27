import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fuzzyMatch, type Range } from "@/lib/fuzzy";
import { boost, recent, recordUse } from "@/lib/frecency";
import type { StoredHost } from "@/lib/workspace";

/**
 * The command palette — UI_BUILD_GUIDE §0.7, "the primary navigation".
 *
 * Requirements from the guide, and how each is met:
 *
 * - **"Must open in under 30ms. Mount it once, hidden, and toggle visibility.
 *   Do not mount on demand."** — `AppShell` keeps this mounted; `open` only
 *   toggles rendering of the overlay, and the candidate list is memoised so
 *   opening does no work beyond a focus call.
 * - **"Recent selections rank first, then fuzzy score. Frecency, not pure
 *   fuzzy."** — `lib/frecency.ts`, added to the fuzzy score.
 * - **"Matched chars highlighted in --amber-500."** — from the same matcher the
 *   host tree uses, so highlighting can never disagree between the two.
 * - **`"> "` prefix switches from object mode to action mode.**
 * - **"Esc closes. Never trap focus."** — and focus returns to whatever had it.
 * - **"First keystroke shows results, no debounce on open."** — matching runs
 *   synchronously; the candidate set is small enough that a frame budget is not
 *   at risk, unlike the 10,000-host tree which needs the debounced Rust path.
 */

export type PaletteAction =
  | { kind: "open-shell"; host: StoredHost; identity: string }
  | { kind: "open-sftp"; host: StoredHost; identity: string }
  | { kind: "local-shell" }
  | { kind: "new-host" }
  | { kind: "edit-host"; host: StoredHost }
  | { kind: "module"; module: string }
  | { kind: "font"; delta: number | "reset" }
  | { kind: "vault" };

interface Item {
  id: string;
  /** Grouping eyebrow, per §0.7 "grouped by kind with a Plex Sans Condensed eyebrow". */
  group: string;
  label: string;
  detail: string;
  /** Only shown in action mode when true, object mode when false. */
  isAction: boolean;
  action: PaletteAction;
}

export function CommandPalette({
  open,
  hosts,
  onClose,
  onAction,
}: {
  open: boolean;
  hosts: readonly StoredHost[];
  onClose: () => void;
  onAction: (a: PaletteAction) => void;
}) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  /** Whatever had focus before we opened, so it can be given back (§0.7). */
  const restoreTo = useRef<HTMLElement | null>(null);

  /* ---- Candidates -------------------------------------------------------- */

  const items = useMemo<Item[]>(() => {
    const out: Item[] = [];

    for (const h of hosts) {
      const ids = h.identities.length > 0 ? h.identities : [];
      for (const idn of ids) {
        out.push({
          id: `ssh:${h.id}:${idn.id}`,
          group: "hosts",
          label: `${h.name} · ${idn.name}`,
          // The searchable text includes user, address and tags, so "prod ssh
          // linux" style queries work as §0.7 describes.
          detail: `ssh ${idn.user}@${h.address}:${h.port} ${h.environment} ${h.tags.join(" ")}`,
          isAction: false,
          action: { kind: "open-shell", host: h, identity: idn.name },
        });
        out.push({
          id: `sftp:${h.id}:${idn.id}`,
          group: "files",
          label: `${h.name} · SFTP·${idn.name}`,
          detail: `sftp ${idn.user}@${h.address} ${h.environment}`,
          isAction: false,
          action: { kind: "open-sftp", host: h, identity: idn.name },
        });
      }
      out.push({
        id: `edit:${h.id}`,
        group: "hosts",
        label: `Edit ${h.name}`,
        detail: `${h.address} settings identities forwards`,
        isAction: true,
        action: { kind: "edit-host", host: h },
      });
    }

    out.push(
      {
        id: "act:local",
        group: "sessions",
        label: "Open local shell",
        detail: "terminal cmd powershell bash",
        isAction: true,
        action: { kind: "local-shell" },
      },
      {
        id: "act:newhost",
        group: "hosts",
        label: "Add host",
        detail: "new create server",
        isAction: true,
        action: { kind: "new-host" },
      },
      {
        id: "act:forwards",
        group: "modules",
        label: "Port forwarding",
        detail: "tunnel socks proxy -L -D",
        isAction: true,
        action: { kind: "module", module: "transfers" },
      },
      {
        id: "act:workspace",
        group: "modules",
        label: "Workspace",
        detail: "hosts tree",
        isAction: true,
        action: { kind: "module", module: "workspace" },
      },
      {
        id: "act:vault",
        group: "modules",
        label: "Vault",
        detail: "secrets passwords credentials unlock master password",
        isAction: true,
        action: { kind: "vault" },
      },
      {
        id: "act:font-in",
        group: "settings",
        label: "Bigger text",
        detail: "font size zoom in",
        isAction: true,
        action: { kind: "font", delta: 1 },
      },
      {
        id: "act:font-out",
        group: "settings",
        label: "Smaller text",
        detail: "font size zoom out",
        isAction: true,
        action: { kind: "font", delta: -1 },
      },
      {
        id: "act:font-reset",
        group: "settings",
        label: "Reset text size",
        detail: "font size default",
        isAction: true,
        action: { kind: "font", delta: "reset" },
      },
    );

    return out;
  }, [hosts]);

  /* ---- Matching ---------------------------------------------------------- */

  const actionMode = query.startsWith(">");
  const term = (actionMode ? query.slice(1) : query).trim().toLowerCase();

  const results = useMemo(() => {
    const pool = actionMode ? items.filter((i) => i.isAction) : items;

    if (term === "") {
      // Nothing typed: frecency order, then everything else. §0.7's "recent
      // selections rank first" is most useful precisely when the query is empty.
      const order = recent(200);
      const rank = new Map(order.map((id, i) => [id, i]));
      return [...pool]
        .sort((a, b) => (rank.get(a.id) ?? 9999) - (rank.get(b.id) ?? 9999))
        .slice(0, 50)
        .map((item) => ({ item, ranges: [] as Range[], score: 0 }));
    }

    const scored: { item: Item; ranges: Range[]; score: number }[] = [];
    for (const item of pool) {
      const hay = `${item.label} ${item.detail}`;
      const m = fuzzyMatch(hay, hay.toLowerCase(), term);
      if (!m) continue;
      // Ranges past the label belong to the detail text and are not rendered;
      // clipping here keeps the highlighter honest about what it is marking.
      const ranges = m.ranges.filter(([, end]) => end <= item.label.length);
      scored.push({ item, ranges, score: m.score + boost(item.id) });
    }
    scored.sort((a, b) => b.score - a.score || a.item.label.localeCompare(b.item.label));
    return scored.slice(0, 50);
  }, [actionMode, items, term]);

  /* ---- Open / close ------------------------------------------------------ */

  useEffect(() => {
    if (!open) return;
    restoreTo.current = document.activeElement as HTMLElement | null;
    setQuery("");
    setCursor(0);
    // Focus after paint, or the overlay is not yet in the DOM.
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, [open]);

  const close = useCallback(() => {
    onClose();
    // §0.7: "Restore focus to the previously focused element on close."
    restoreTo.current?.focus?.();
  }, [onClose]);

  const activate = useCallback(
    (index: number) => {
      const hit = results[index];
      if (!hit) return;
      recordUse(hit.item.id);
      onAction(hit.item.action);
      close();
    },
    [close, onAction, results],
  );

  useEffect(() => {
    if (cursor >= results.length) setCursor(Math.max(0, results.length - 1));
  }, [cursor, results.length]);

  // Keep the highlighted row in view without smooth scrolling, which lags
  // behind held arrow keys.
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-idx="${cursor}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  if (!open) return null;

  return (
    <div
      data-overlay
      className="absolute inset-0 z-[60] flex items-start justify-center bg-chassis-900/70 pt-[12vh]"
      onClick={close}
    >
      <div
        role="dialog"
        aria-label="Command palette"
        onClick={(e) => e.stopPropagation()}
        className="flex max-h-[64vh] w-[620px] flex-col border-hairline border-bezel-500 bg-chassis-800"
      >
        <input
          ref={inputRef}
          value={query}
          spellCheck={false}
          placeholder="Search hosts, or type > for actions"
          onChange={(e) => {
            setQuery(e.target.value);
            setCursor(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              close();
              e.preventDefault();
            } else if (e.key === "ArrowDown") {
              setCursor((c) => Math.min(results.length - 1, c + 1));
              e.preventDefault();
            } else if (e.key === "ArrowUp") {
              setCursor((c) => Math.max(0, c - 1));
              e.preventDefault();
            } else if (e.key === "Enter") {
              activate(cursor);
              e.preventDefault();
            } else if (e.key === "Tab") {
              // §0.7: "⇥ completes to the highlighted item."
              const hit = results[cursor];
              if (hit) setQuery((actionMode ? "> " : "") + hit.item.label);
              e.preventDefault();
            }
            e.stopPropagation();
          }}
          // §0.7: 20px Plex Mono input.
          className="num border-b-hairline border-bezel-500 bg-transparent px-4 py-3 text-display
                     text-text-hi placeholder:text-text-lo focus:outline-none"
        />

        <div ref={listRef} role="listbox" className="min-h-0 flex-1 overflow-auto py-1">
          {results.length === 0 && (
            <p className="px-4 py-3 text-small text-text-lo">
              {hosts.length === 0
                ? "No saved hosts yet — type > to see actions."
                : "No matches."}
            </p>
          )}

          {results.map(({ item, ranges }, i) => {
            const showGroup = i === 0 || results[i - 1]?.item.group !== item.group;
            return (
              <div key={item.id}>
                {showGroup && (
                  <p className="px-4 pb-[2px] pt-2 font-condensed uppercase text-eyebrow text-text-lo">
                    {item.group}
                  </p>
                )}
                <div
                  role="option"
                  aria-selected={i === cursor}
                  data-idx={i}
                  onMouseEnter={() => setCursor(i)}
                  onClick={() => activate(i)}
                  className={`flex cursor-default items-baseline gap-3 px-4 py-[3px] ${
                    i === cursor ? "bg-chassis-700" : ""
                  }`}
                >
                  <span className="min-w-0 flex-1 truncate text-small text-text-hi">
                    <Highlighted text={item.label} ranges={ranges} />
                  </span>
                  <span className="num shrink-0 truncate text-micro text-text-lo">
                    {item.detail}
                  </span>
                </div>
              </div>
            );
          })}
        </div>

        <footer className="flex gap-4 border-t-hairline border-bezel-500 px-4 py-1 text-micro text-text-lo">
          <span>↑↓ navigate</span>
          <span>⏎ open</span>
          <span>⇥ complete</span>
          <span>&gt; actions</span>
          <span className="ml-auto">esc close</span>
        </footer>
      </div>
    </div>
  );
}

/** §0.7: "Fuzzy-highlight matched characters in --amber-500." */
function Highlighted({ text, ranges }: { text: string; ranges: readonly Range[] }) {
  if (ranges.length === 0) return <>{text}</>;
  const parts: React.ReactNode[] = [];
  let cursor = 0;
  ranges.forEach(([start, end], i) => {
    if (start > cursor) parts.push(text.slice(cursor, start));
    parts.push(
      <mark key={i} className="bg-transparent text-amber-500">
        {text.slice(start, end)}
      </mark>,
    );
    cursor = end;
  });
  if (cursor < text.length) parts.push(text.slice(cursor));
  return <>{parts}</>;
}
