import { useCallback, useMemo, useRef } from "react";
import type { ConnectionKind, Environment, Host, HostStatus } from "@/types";
import type { Range } from "@/lib/fuzzy";
import { charBudget, middleTruncate } from "@/lib/truncate";
import { PortChip } from "./PortChip";

/**
 * The host row — a faceplate. §0.6 specs it precisely because it is the
 * most-used component in the product.
 *
 * ROW HEIGHT: §0.9 overrides §0.6 explicitly — "Figma is authoritative and this
 * guide is stale: §0.6 says the host row is 44px two-line. The Figma build is
 * ~64px three-line, because the port chips *are* the product thesis and hiding
 * them defeats the point. Follow Figma." So the default is 64px / three lines,
 * and 44px two-line is not built. Compact mode (26px, chips hidden) is built,
 * because §0.6 is right that 200-host users need it.
 */

export const ROW_H_DEFAULT = 64;
export const ROW_H_COMPACT = 26;

const ENV_VAR: Record<Environment, string> = {
  prod: "var(--env-prod)",
  stage: "var(--env-stage)",
  dev: "var(--env-dev)",
  test: "var(--env-test)",
  customer: "var(--env-customer)",
};

export interface HostRowProps {
  host: Host;
  compact?: boolean;
  selected?: boolean;
  /** Highlight spans from `filter_hosts`, over `field`. */
  highlight?: { field: "name" | "address"; ranges: readonly Range[] };
  /** Width available for the row, px — drives middle truncation of the address. */
  width: number;
  onSelect?: (host: Host) => void;
  onOpenConnection?: (host: Host, kind: ConnectionKind, identityName?: string) => void;
  /** Shown as a ⋮ affordance on hover when the host is editable. */
  onEdit?: () => void;
}

export function HostRow({
  host,
  compact = false,
  selected = false,
  highlight,
  width,
  onSelect,
  onOpenConnection,
  onEdit,
}: HostRowProps) {
  const chips = useRef<(HTMLButtonElement | null)[]>([]);
  const rowRef = useRef<HTMLDivElement | null>(null);

  const addressLine = `${host.address} · ${host.os}`;
  const truncated = useMemo(() => {
    // Subtract the 3px tie, horizontal padding, and the status-dot column.
    const available = width - 3 - 20 - 24;
    return middleTruncate(addressLine, charBudget(available));
  }, [addressLine, width]);

  /**
   * §Step 4: "row focusable, arrow keys move between chips, Enter activates".
   * The row owns a single tab stop; chips are reached with ← →, which keeps the
   * tab order through a 10,000-row tree usable. Without this a keyboard user
   * would tab through four chips per host.
   */
  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      const list = chips.current.filter(Boolean) as HTMLButtonElement[];
      if (list.length === 0 && e.key !== "Enter") return;

      const active = document.activeElement as HTMLElement | null;
      const at = list.findIndex((el) => el === active);

      if (e.key === "ArrowRight") {
        list[Math.min(list.length - 1, at + 1)]?.focus();
      } else if (e.key === "ArrowLeft") {
        if (at <= 0) rowRef.current?.focus();
        else list[at - 1]?.focus();
      } else if (e.key === "Enter") {
        if (at >= 0) list[at]?.click();
        else onSelect?.(host);
      } else {
        return;
      }
      e.preventDefault();
      e.stopPropagation(); // don't let the tree's up/down handler also fire
    },
    [host, onSelect],
  );

  return (
    <div
      ref={rowRef}
      role="option"
      aria-selected={selected}
      aria-label={`${host.name}, ${host.environment}, ${STATUS_LABEL[host.status]}`}
      tabIndex={0}
      onClick={() => onSelect?.(host)}
      onKeyDown={onKeyDown}
      style={{ height: compact ? ROW_H_COMPACT : ROW_H_DEFAULT }}
      className="row-hoverable group relative flex w-full cursor-default items-stretch overflow-hidden pl-[3px]"
    >
      {/* §0.2: environment colour appears in EXACTLY ONE place per row — a 3px
          left edge bar, full row height. Never a background tint: that destroys
          terminal contrast. `data-testid` is asserted by visual-check.spec.ts at
          100/125/150% scaling, where a hairline would otherwise vanish. */}
      <span
        data-testid="env-tie"
        aria-hidden
        className="absolute inset-y-0 left-0 w-[3px]"
        style={{ background: ENV_VAR[host.environment] }}
      />

      {compact ? (
        <div className="flex min-w-0 flex-1 items-center gap-2 px-2">
          <StatusDot status={host.status} />
          <span className="min-w-0 truncate text-small font-medium text-text-hi">
            <Highlighted
              text={host.name}
              ranges={highlight?.field === "name" ? highlight.ranges : undefined}
            />
          </span>
          <span className="num ml-auto shrink-0 text-micro text-text-lo">{host.address}</span>
        </div>
      ) : (
        <div className="flex min-w-0 flex-1 flex-col justify-center gap-[1px] px-2 py-1">
          <div className="flex min-w-0 items-center gap-2">
            <span className="min-w-0 truncate text-small font-medium text-text-hi">
              <Highlighted
                text={host.name}
                ranges={highlight?.field === "name" ? highlight.ranges : undefined}
              />
            </span>
            <span className="ml-auto flex shrink-0 items-center gap-1">
              <StatusDot status={host.status} />
              {onEdit && (
                <button
                  type="button"
                  tabIndex={-1}
                  aria-label={`Edit ${host.name}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    onEdit();
                  }}
                  className="text-micro text-text-lo opacity-0 transition-opacity duration-hover
                             group-hover:opacity-100 hover:text-amber-500"
                >
                  ⋮
                </button>
              )}
            </span>
          </div>

          <div className="num truncate text-micro text-text-lo">
            {highlight?.field === "address" ? (
              <Highlighted text={host.address} ranges={highlight.ranges} />
            ) : (
              truncated
            )}
          </div>

          <div className="flex min-w-0 items-center gap-1 overflow-hidden">
            {host.connections.length === 0 ? (
              <span className="text-micro text-text-lo">no connections</span>
            ) : (
              host.connections.map((c, i) => (
                <PortChip
                  key={`${c.kind}:${c.identityName ?? ""}`}
                  connection={c}
                  tabIndex={-1}
                  chipRef={(el) => {
                    chips.current[i] = el;
                  }}
                  onOpen={() => onOpenConnection?.(host, c.kind, c.identityName)}
                />
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/* ---- Status dot ---------------------------------------------------------- */

const STATUS_LABEL: Record<HostStatus, string> = {
  connected: "connected",
  idle: "idle",
  failed: "last attempt failed",
  connecting: "connecting",
};

/**
 * §0.6: 6px. Filled cyan = connected, hollow --text-lo = idle, filled red =
 * failed, amber pulsing = connecting.
 *
 * Cyan here is link state, which is the ONE thing §0.2 permits it for — this dot
 * is why "connected" must never compete with a cyan button.
 */
function StatusDot({ status }: { status: HostStatus }) {
  const base = "inline-block h-[6px] w-[6px] rounded-full";
  const styles: Record<HostStatus, string> = {
    connected: "bg-cyan-400",
    idle: "border-hairline border-text-lo bg-transparent",
    failed: "bg-red-500",
    connecting: "bg-amber-500 animate-pulse",
  };
  return (
    <span
      role="img"
      aria-label={STATUS_LABEL[status]}
      className={`${base} ${styles[status]}`}
    />
  );
}

/* ---- Match highlighting -------------------------------------------------- */

/**
 * §0.7 / Step 5: "Highlight matched characters in --amber-500 using the returned
 * ranges." Ranges are byte-for-byte indices from `filter_hosts`, so this never
 * re-runs the match in the render path.
 */
function Highlighted({ text, ranges }: { text: string; ranges: readonly Range[] | undefined }) {
  if (!ranges || ranges.length === 0) return <>{text}</>;

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
