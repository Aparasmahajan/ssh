import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { MOD_LABEL, keyHint } from "@/lib/platform";
import { IS_DESKTOP, desktopVersion } from "@/lib/runtime";
import { humanSize } from "@/lib/remote";
import { usePanes, type Node } from "@/state/panes";
import { useSessions } from "@/state/sessions";
import { vaultStatus, type VaultStatus } from "@/lib/vault";

/**
 * UI_BUILD_GUIDE §0.5 zone 5 — 28px, full width.
 *
 * **Every number here is measured.** An earlier version shipped plausible
 * constants — `3.2 ms`, `12.4 MB/s`, `chacha20-poly1305`, `7 sessions` — which
 * is worse than showing nothing: a fabricated throughput figure in a tool used
 * to manage production teaches the user to distrust the whole rail.
 *
 * Where something genuinely cannot be measured yet it shows an em dash, and the
 * tooltip says why. The RTT readout in particular is the **connect** round trip,
 * not §4's live per-keystroke RTT, and it is labelled `connect` so it cannot be
 * mistaken for one.
 */

interface AppStats {
  sessions: number;
  bytes_in: number;
  bytes_out: number;
  connections: number;
  cipher: string | null;
  kex: string | null;
  host_key: string | null;
  connect_rtt_ms: number | null;
  forwards: number;
  forward_bytes_in: number;
  forward_bytes_out: number;
}

const EMPTY: AppStats = {
  sessions: 0,
  bytes_in: 0,
  bytes_out: 0,
  connections: 0,
  cipher: null,
  kex: null,
  host_key: null,
  connect_rtt_ms: null,
  forwards: 0,
  forward_bytes_in: 0,
  forward_bytes_out: 0,
};

/** §13.1 rule 8 samples at 10 Hz. The rail is glanceable, not a graph, so 4 Hz
 *  is plenty and costs less. */
const POLL_MS = 250;

/** §14.6: "Smooth rate readouts with a 1 s EWMA and `tabular-nums`. A number
 *  jittering between 94 and 108 MB/s reads as broken even when it isn't."
 *  At 4 Hz, alpha = dt / tau = 0.25. */
const EWMA_ALPHA = POLL_MS / 1000;

export function StatusRail({ onOpenVault }: { onOpenVault?: () => void }) {
  const [stats, setStats] = useState<AppStats>(EMPTY);
  const [vault, setVault] = useState<VaultStatus | null>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [rate, setRate] = useState({ in: 0, out: 0 });
  /** Previous sample, for the delta. Held in a ref so it never causes a render. */
  const prev = useRef<{ t: number; in: number; out: number } | null>(null);

  useEffect(() => {
    if (!IS_DESKTOP) return;
    void desktopVersion().then(setVersion);

    const tick = () => {
      invoke<AppStats>("app_stats")
        .then((s) => {
          setStats(s);

          const now = performance.now();
          const totIn = s.bytes_in + s.forward_bytes_in;
          const totOut = s.bytes_out + s.forward_bytes_out;
          const last = prev.current;
          prev.current = { t: now, in: totIn, out: totOut };
          if (!last) return;

          const dt = (now - last.t) / 1000;
          if (dt <= 0) return;
          // Counters only ever grow; a negative delta means the process
          // restarted, so start the average again rather than emit a spike.
          const dIn = Math.max(0, totIn - last.in) / dt;
          const dOut = Math.max(0, totOut - last.out) / dt;
          setRate((r) => ({
            in: r.in + EWMA_ALPHA * (dIn - r.in),
            out: r.out + EWMA_ALPHA * (dOut - r.out),
          }));
        })
        .catch(() => undefined);
    };
    tick();
    const t = window.setInterval(tick, POLL_MS);
    return () => window.clearInterval(t);
  }, []);

  // The vault auto-locks on an idle timer in the backend, so this has to be
  // polled — a one-shot read would leave the rail showing "unlocked" long after
  // it stopped being true. Once every 5s, not at POLL_MS: nothing here changes
  // at 60 Hz and the status call touches a mutex the connect path also wants.
  useEffect(() => {
    if (!IS_DESKTOP) return;
    const read = () => void vaultStatus().then(setVault).catch(() => undefined);
    read();
    const t = window.setInterval(read, 5000);
    return () => window.clearInterval(t);
  }, []);

  const totalIn = stats.bytes_in + stats.forward_bytes_in;
  const totalOut = stats.bytes_out + stats.forward_bytes_out;

  // Step 9: broadcast needs "a persistent amber pill in the status rail reading
  // BROADCAST - 4 panes", because typing one command into four production
  // servers by accident is the worst thing this app could allow.
  const activeTab = useSessions((s) => s.activeId);
  const broadcasting = usePanes((s) => (activeTab ? (s.broadcast[activeTab] ?? false) : false));
  const paneCount = usePanes((s) => {
    if (!activeTab) return 0;
    const tree = s.trees[activeTab];
    if (!tree) return 0;
    const count = (n: Node): number => (n.type === "leaf" ? 1 : count(n.a) + count(n.b));
    return count(tree);
  });

  return (
    <footer
      data-testid="status-rail"
      className="flex h-status items-center gap-5 border-t-hairline border-bezel-500
                 bg-chassis-800 px-3 text-micro text-text-mid"
    >
      {/* Vault state. Shown only once a vault exists, so it is not a
          permanent nag for anyone who never creates one. */}
      {vault?.exists && (
        <button
          onClick={onOpenVault}
          title={
            vault.unlocked
              ? `Vault unlocked - ${vault.records} secret(s). Auto-locks after ${Math.round(
                  vault.idleTimeoutSecs / 60,
                )} min idle.`
              : "Vault locked. Click to unlock."
          }
          className={`num shrink-0 rounded border-hairline px-[6px] py-[1px] ${
            vault.unlocked
              ? "border-amber-500 text-amber-500"
              : "border-bezel-500 text-text-lo hover:text-text-hi"
          }`}
        >
          {vault.unlocked ? `vault ${vault.records}` : "vault locked"}
        </button>
      )}

      {broadcasting && (
        <span
          className="num rounded border-hairline border-amber-500 bg-amber-900 px-[6px]
                     uppercase text-micro text-amber-500"
          title="Every keystroke goes to every pane in this tab. Ctrl+Shift+B to stop."
        >
          broadcast · {paneCount} panes
        </span>
      )}

      <Readout
        label="sessions"
        value={String(stats.sessions)}
        title="Open terminal sessions, local and remote"
        live
      />

      <Readout
        label="hosts"
        value={String(stats.connections)}
        title="Live SSH transports. One per (host, port, user, identity)."
        live
      />

      {/* Rate, smoothed with a 1-second EWMA per §14.6. Below 1 KB/s it reads
          "idle" rather than flickering through single-digit byte counts. */}
      <Readout
        label="in"
        value={`↓ ${formatRate(rate.in)}`}
        title={`Total received: ${humanSize(totalIn)}`}
        live
      />
      <Readout
        label="out"
        value={`↑ ${formatRate(rate.out)}`}
        title={`Total sent: ${humanSize(totalOut)}`}
        live
      />

      {/* Honest about a library limitation rather than displaying a guess:
          russh 0.62 does not expose the negotiated algorithms to a client
          (`negotiation::Names` is private and there is no accessor), so this
          reads "n/a" whenever a connection is up. See COMPLETED.md. */}
      <Readout
        label="cipher"
        value={stats.cipher ?? (stats.connections > 0 ? "n/a" : "—")}
        title={
          stats.cipher
            ? `Negotiated: ${stats.cipher}`
            : stats.connections > 0
              ? "Connected, but russh 0.62 does not expose the negotiated cipher to clients. Showing a guess here would be worse than showing nothing."
              : "No SSH connection — a local shell is not encrypted, it is a pipe"
        }
      />

      <Readout
        label="connect"
        value={stats.connect_rtt_ms === null ? "—" : `${stats.connect_rtt_ms} ms`}
        title="TCP connect + handshake time. Not a live per-keystroke RTT — that needs a probe that does not exist yet."
        live
      />

      {stats.forwards > 0 && (
        <Readout label="forwards" value={String(stats.forwards)} title="Running port forwards" live />
      )}

      <span className="ml-auto flex items-center gap-3">
        {IS_DESKTOP ? (
          <span className="num text-micro text-text-lo">{version ? `v${version}` : ""}</span>
        ) : (
          <span
            title="Running in a browser. The terminal needs the Rust core."
            className="num rounded border-hairline border-red-500 px-[5px] uppercase text-micro text-red-500"
          >
            browser · no terminal
          </span>
        )}
        <kbd className="num border-hairline border-bezel-500 px-1 py-[1px] text-micro text-text-lo">
          {keyHint(MOD_LABEL, "K")}
        </kbd>
      </span>
    </footer>
  );
}

/** Rates below 1 KB/s are noise; showing them jitters the column for nothing. */
function formatRate(bytesPerSecond: number): string {
  if (bytesPerSecond < 1024) return "idle";
  return `${humanSize(bytesPerSecond)}/s`;
}

function Readout({
  label,
  value,
  title,
  live,
}: {
  label: string;
  value: string;
  title: string;
  live?: boolean;
}) {
  return (
    <span className="flex items-baseline gap-[6px]" title={title}>
      <span className="font-condensed uppercase text-eyebrow text-text-lo">{label}</span>
      <span className="num text-micro text-text-mid" {...(live ? { "data-live": "" } : {})}>
        {value}
      </span>
    </span>
  );
}
