import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { ConnectionKind, Environment, Host } from "@/types";
import type { Range } from "@/lib/fuzzy";
import {
  FIELD_ADDRESS,
  buildHostIndex,
  filterHosts,
  rangesFor,
  type FilterResult,
} from "@/lib/filterHosts";
import { HostRow, ROW_H_COMPACT, ROW_H_DEFAULT } from "./HostRow";

/**
 * The workspace tree — UI_BUILD_GUIDE §0.5 zone 2, Step 5.
 *
 * Budget: 10,000 hosts, keystroke → repaint under 5ms (Step 5) / 3ms
 * (CLAUDE.md §4). Four structural decisions, not micro-optimizations
 * (CLAUDE.md §1):
 *
 *   1. The tree is FLATTENED to a VisibleNode[] on expand/collapse or filter
 *      change. Render never recurses (CLAUDE.md §7: "Never recurse during paint").
 *   2. Only visible rows mount, via @tanstack/react-virtual.
 *   3. Matching happens behind the `filter_hosts` seam — bit-parallel prefilter,
 *      SoA results, counting sort — debounced one frame, previous query
 *      cancelled, and each extra character reuses the prior candidate set.
 *   4. Highlight ranges are computed for VISIBLE ROWS ONLY, in render, from the
 *      ~30 mounted rows rather than all 10,000 results.
 */

const GROUP_H = 22;
const DEBOUNCE_MS = 16; // one frame — Step 5
const OVERSCAN = 8;

/** Fixed order: prod first, because that is where mistakes are expensive. */
const ENV_ORDER: Environment[] = ["prod", "stage", "dev", "test", "customer"];
const ENV_LABEL: Record<Environment, string> = {
  prod: "production",
  stage: "staging",
  dev: "development",
  test: "test",
  customer: "customer",
};
const ENV_VAR: Record<Environment, string> = {
  prod: "var(--env-prod)",
  stage: "var(--env-stage)",
  dev: "var(--env-dev)",
  test: "var(--env-test)",
  customer: "var(--env-customer)",
};

type VisibleNode =
  | { type: "group"; env: Environment; count: number; collapsed: boolean }
  | { type: "host"; host: Host; hostIdx: number; field: number };

export function WorkspaceTree({
  hosts,
  width,
  compact = false,
  onOpenConnection,
  onAddHost,
  onEditHost,
}: {
  hosts: readonly Host[];
  width: number;
  compact?: boolean;
  onOpenConnection?: (host: Host, kind: ConnectionKind, identityName?: string) => void;
  onAddHost?: () => void;
  onEditHost?: (hostId: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [result, setResult] = useState<FilterResult | null>(null); // null = unfiltered
  const [collapsed, setCollapsed] = useState<ReadonlySet<Environment>>(new Set());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeIndex, setActiveIndex] = useState(-1);

  const index = useMemo(() => buildHostIndex(hosts), [hosts]);

  /* ---- Query pipeline: debounce one frame, cancel in flight, reuse prefix -- */
  // Held in a ref, not state: it feeds the NEXT query as an optimization hint
  // and must not itself trigger a render.
  const prevResult = useRef<FilterResult | undefined>(undefined);
  useEffect(() => {
    prevResult.current = undefined;
  }, [index]);

  useEffect(() => {
    if (query.trim() === "") {
      prevResult.current = undefined;
      setResult(null);
      return;
    }
    const controller = new AbortController();
    const timer = window.setTimeout(() => {
      filterHosts(index, query, [], controller.signal, prevResult.current)
        .then((r) => {
          prevResult.current = r;
          setResult(r);
        })
        .catch((err: unknown) => {
          // An aborted query is the normal case, not an error: every keystroke
          // cancels the previous one (Step 5).
          if (!(err instanceof DOMException && err.name === "AbortError")) throw err;
        });
    }, DEBOUNCE_MS);

    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [index, query]);

  /* ---- Flatten ----------------------------------------------------------- */
  const nodes = useMemo(() => flatten(hosts, result, collapsed), [hosts, result, collapsed]);
  const matchCount = result?.count ?? hosts.length;
  const activeQuery = result?.query ?? "";

  /* ---- Virtualize -------------------------------------------------------- */
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const rowH = compact ? ROW_H_COMPACT : ROW_H_DEFAULT;
  const virtualizer = useVirtualizer({
    count: nodes.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => ((nodes[i] as VisibleNode).type === "group" ? GROUP_H : rowH),
    overscan: OVERSCAN,
  });

  const toggleGroup = useCallback((env: Environment) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(env)) next.delete(env);
      else next.add(env);
      return next;
    });
  }, []);

  /* ---- Keyboard: ↑ ↓ across the flattened list --------------------------- */
  const onListKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      const delta = e.key === "ArrowDown" ? 1 : -1;
      const next = Math.min(nodes.length - 1, Math.max(0, activeIndex + delta));
      setActiveIndex(next);
      virtualizer.scrollToIndex(next, { align: "auto" });
      e.preventDefault();
    },
    [activeIndex, nodes.length, virtualizer],
  );

  // Focus follows activeIndex, after the virtualizer has mounted the row.
  useEffect(() => {
    if (activeIndex < 0) return;
    const el = scrollRef.current?.querySelector<HTMLElement>(`[data-node-index="${activeIndex}"]`);
    el?.focus();
  }, [activeIndex, nodes]);

  return (
    <aside
      data-testid="workspace-tree"
      style={{ width }}
      className="flex min-w-0 flex-col overflow-hidden bg-chassis-800"
    >
      <div className="flex items-center gap-2 border-b-hairline border-bezel-500 px-2 py-[6px]">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query !== "") {
              setQuery("");
              e.stopPropagation(); // Esc clears the filter before it exits zoom
            }
          }}
          aria-label="Filter hosts"
          placeholder="filter hosts"
          spellCheck={false}
          className="min-w-0 flex-1 border-none bg-transparent text-small text-text-hi
                     placeholder:text-text-lo focus:outline-none"
        />
        <span className="num shrink-0 text-micro text-text-lo" data-live>
          {matchCount}
        </span>
        {onAddHost && (
          <button
            type="button"
            onClick={onAddHost}
            aria-label="Add host"
            title="Add host"
            className="shrink-0 text-large leading-none text-text-lo hover:text-amber-500"
          >
            +
          </button>
        )}
      </div>

      <div
        ref={scrollRef}
        role="listbox"
        aria-label="Hosts"
        onKeyDown={onListKeyDown}
        className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden"
      >
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((v) => {
            const node = nodes[v.index] as VisibleNode;
            return (
              <div
                key={v.key}
                data-node-index={v.index}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  height: v.size,
                  transform: `translateY(${v.start}px)`,
                }}
              >
                {node.type === "group" ? (
                  <GroupHeader
                    env={node.env}
                    count={node.count}
                    collapsed={node.collapsed}
                    onToggle={() => toggleGroup(node.env)}
                  />
                ) : (
                  <HostRow
                    host={node.host}
                    width={width}
                    compact={compact}
                    selected={selectedId === node.host.id}
                    {...highlightProps(index, node, activeQuery)}
                    onSelect={(h) => setSelectedId(h.id)}
                    {...(onEditHost ? { onEdit: () => onEditHost(node.host.id) } : {})}
                    {...(onOpenConnection ? { onOpenConnection } : {})}
                  />
                )}
              </div>
            );
          })}
        </div>
      </div>
    </aside>
  );
}

/**
 * Ranges for one visible row. This is the whole reason `filter_hosts` returns
 * scores without ranges: it runs about 30 times per frame, not 10,000.
 */
function highlightProps(
  index: ReturnType<typeof buildHostIndex>,
  node: Extract<VisibleNode, { type: "host" }>,
  query: string,
): Partial<{ highlight: { field: "name" | "address"; ranges: readonly Range[] } }> {
  if (query === "") return {};
  const ranges = rangesFor(index, node.hostIdx, node.field, query);
  if (ranges.length === 0) return {};
  return { highlight: { field: node.field === FIELD_ADDRESS ? "address" : "name", ranges } };
}

function GroupHeader({
  env,
  count,
  collapsed,
  onToggle,
}: {
  env: Environment;
  count: number;
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={!collapsed}
      style={{ height: GROUP_H }}
      className="row-hoverable relative flex w-full items-center gap-2 pl-[3px] pr-2 text-left"
    >
      <span
        aria-hidden
        className="absolute inset-y-0 left-0 w-[3px]"
        style={{ background: ENV_VAR[env] }}
      />
      <span className="ml-2 font-condensed uppercase text-eyebrow text-text-mid">
        {ENV_LABEL[env]}
      </span>
      <span className="num ml-auto text-eyebrow text-text-lo">{count}</span>
      <span aria-hidden className="text-micro text-text-lo">
        {collapsed ? "▶" : "▼"}
      </span>
    </button>
  );
}

/**
 * Flatten to a linear list. O(n) bucketing, plus one sort per group only when
 * unfiltered — filtered order comes ranked out of the counting sort, so it is
 * preserved as-is. This is the only place tree structure exists; nothing below
 * it recurses, which keeps paint cost proportional to the viewport rather than
 * the host count.
 */
function flatten(
  hosts: readonly Host[],
  result: FilterResult | null,
  collapsed: ReadonlySet<Environment>,
): VisibleNode[] {
  const buckets = new Map<Environment, Extract<VisibleNode, { type: "host" }>[]>();
  const push = (host: Host, hostIdx: number, field: number) => {
    let list = buckets.get(host.environment);
    if (!list) buckets.set(host.environment, (list = []));
    list.push({ type: "host", host, hostIdx, field });
  };

  if (result) {
    // `order` holds host indices straight into `hosts` — no id→host map to
    // rebuild per keystroke.
    for (let k = 0; k < result.count; k++) {
      const i = result.order[k] as number;
      const host = hosts[i];
      if (host) push(host, i, result.fields[k] as number);
    }
  } else {
    for (let i = 0; i < hosts.length; i++) push(hosts[i] as Host, i, 0);
    // Codepoint order, not localeCompare — this runs on every expand/collapse,
    // and collation is both slow and platform-dependent.
    for (const list of buckets.values()) {
      list.sort((a, b) => (a.host.name < b.host.name ? -1 : a.host.name > b.host.name ? 1 : 0));
    }
  }

  const out: VisibleNode[] = [];
  for (const env of ENV_ORDER) {
    const list = buckets.get(env);
    if (!list || list.length === 0) continue; // an empty group is noise, not information
    const isCollapsed = collapsed.has(env);
    out.push({ type: "group", env, count: list.length, collapsed: isCollapsed });
    if (isCollapsed) continue;
    for (const entry of list) out.push(entry);
  }
  return out;
}
