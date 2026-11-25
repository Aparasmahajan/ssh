import { useEffect, useRef, useState } from "react";
import {
  humanSize,
  TRANSFER_QUEUED,
  transferCancel,
  transferClear,
  transferList,
  transferStart,
  type Transfer,
} from "@/lib/remote";

/**
 * The transfer queue for one connection (CLAUDE.md §6).
 *
 * Sampled, never pushed (§13.1 rule 8): 10 Hz while anything is moving, 1 Hz
 * when idle. Rates are a 1 s EWMA in tabular figures (§14.6), because a number
 * jittering between 94 and 108 MB/s reads as broken even when it isn't.
 *
 * The method is always shown (§6.2: "Surface the switch in the UI"). A user who
 * waited nine minutes in WinSCP deserves to know why this took eleven seconds.
 */
export function TransferList({
  connectionKey,
  onFinished,
}: {
  connectionKey: string;
  /** A transfer on this connection finished: re-list the panes. */
  onFinished: () => void;
}) {
  const [jobs, setJobs] = useState<Transfer[]>([]);
  const [error, setError] = useState<string | null>(null);
  const rates = useRef(new Map<number, { bytes: number; at: number; rate: number }>());
  const seen = useRef(new Map<number, string>());
  const finished = useRef(onFinished);
  finished.current = onFinished;

  useEffect(() => {
    let live = true;
    let timer = 0;
    // The first sample only records what already exists: jobs finished
    // before this list mounted must not trigger a refresh.
    let primed = false;
    // One sampling loop only: a wake during a sample asks for one more sample
    // straight after, instead of starting a second loop.
    let busy = false;
    let again = false;
    const tick = async () => {
      if (busy) {
        again = true;
        return;
      }
      busy = true;
      let active = false;
      try {
        const all = (await transferList()).filter((j) => j.key === connectionKey);
        if (!live) return;
        const now = performance.now();
        let done = false;
        for (const j of all) {
          // 1 s time-constant EWMA, robust to uneven sample spacing.
          const r = rates.current.get(j.id);
          if (!r) {
            rates.current.set(j.id, { bytes: j.bytes_done, at: now, rate: 0 });
          } else if (now > r.at) {
            const dt = (now - r.at) / 1000;
            const inst = (j.bytes_done - r.bytes) / dt;
            const a = 1 - Math.exp(-dt / 1);
            rates.current.set(j.id, { bytes: j.bytes_done, at: now, rate: r.rate + a * (inst - r.rate) });
          }
          // Any job reaching "done" after mount counts, including one first
          // seen already done: a small job can start and finish between two
          // idle samples, and then the panes never re-listed.
          const before = seen.current.get(j.id);
          if (primed && before !== "done" && j.state === "done") done = true;
          seen.current.set(j.id, j.state);
          if (j.state === "queued" || j.state === "running") active = true;
        }
        setJobs(all);
        setError(null);
        primed = true;
        if (done) finished.current();
      } catch (e) {
        if (live) setError(String(e));
      }
      busy = false;
      if (!live) return;
      if (again) {
        again = false;
        void tick();
        return;
      }
      timer = window.setTimeout(() => void tick(), active ? 100 : 1000);
    };
    // A queued transfer: sample now rather than at the next idle tick.
    const wake = () => {
      window.clearTimeout(timer);
      void tick();
    };
    window.addEventListener(TRANSFER_QUEUED, wake);
    void tick();
    return () => {
      live = false;
      window.clearTimeout(timer);
      window.removeEventListener(TRANSFER_QUEUED, wake);
    };
  }, [connectionKey]);

  if (jobs.length === 0 && !error) return null;
  const anyFinished = jobs.some((j) => j.state !== "queued" && j.state !== "running");

  return (
    <section
      data-testid="transfer-list"
      className="max-h-[34%] shrink-0 overflow-y-auto border-t-hairline border-bezel-500 bg-chassis-800"
    >
      <header className="sticky top-0 flex items-center gap-2 bg-chassis-800 px-3 py-1">
        <span className="font-condensed uppercase text-eyebrow text-text-lo">transfers</span>
        {error && <span className="num text-micro text-red-500">{error}</span>}
        {anyFinished && (
          <button
            type="button"
            onClick={() => void transferClear()}
            className="ml-auto text-micro text-text-lo hover:text-text-hi"
          >
            clear finished
          </button>
        )}
      </header>
      <ul>
        {[...jobs].reverse().map((j) => (
          <Row key={j.id} job={j} rate={rates.current.get(j.id)?.rate ?? 0} />
        ))}
      </ul>
    </section>
  );
}

function Row({ job: j, rate }: { job: Transfer; rate: number }) {
  const running = j.state === "running";
  const pct = j.bytes_total > 0 ? Math.min(100, (100 * j.bytes_done) / j.bytes_total) : 0;
  const eta = running && rate > 0 && j.bytes_total > j.bytes_done ? (j.bytes_total - j.bytes_done) / rate : null;
  const multi = j.files_total > 1;
  const tone =
    j.state === "failed"
      ? "text-red-500"
      : j.state === "done"
        ? "text-cyan-400"
        : j.state === "cancelled"
          ? "text-text-lo"
          : "text-text-mid";

  return (
    <li className="grid grid-cols-[16px_minmax(0,1fr)_auto] items-center gap-x-2 px-3 py-[3px] text-micro">
      <span className={tone} aria-label={j.direction}>
        {j.direction === "upload" ? "↑" : "↓"}
      </span>
      <div className="min-w-0">
        <div className="flex items-baseline gap-2">
          <span className="truncate text-text-hi" title={`${j.source} → ${j.target_dir}`}>
            {j.name}
          </span>
          {/* No chip for a job that never got as far as choosing a method
              (refused before starting: target exists, bad path). */}
          {!(j.method === "preparing" && j.state !== "running") && (
            <span
              className="num shrink-0 rounded-sm border-hairline border-bezel-500 px-1 text-text-lo"
              title={j.note ?? (j.window > 1 ? `peak request window ${j.window}` : undefined)}
            >
              {j.method}
              {j.note && <span className="text-amber-500"> · fallback</span>}
            </span>
          )}
          <span className={`num ml-auto shrink-0 ${tone}`}>
            {j.state === "running"
              ? `${pct.toFixed(0)}%`
              : j.state === "done"
                ? `done · ${seconds(j.elapsed_ms)}`
                : j.state}
          </span>
        </div>
        {(running || j.state === "done") && (
          <div className="mt-[2px] h-[2px] w-full bg-bezel-500">
            <div
              className={j.state === "done" ? "h-full bg-cyan-400" : "h-full bg-amber-500"}
              style={{ width: `${j.state === "done" ? 100 : pct}%` }}
            />
          </div>
        )}
        <div className="num mt-[1px] flex gap-3 text-text-lo">
          <span>
            {humanSize(j.bytes_done)}
            {j.bytes_total > 0 && ` / ${humanSize(j.bytes_total)}`}
          </span>
          {multi && (
            <span>
              {j.files_done.toLocaleString()} / {j.files_total.toLocaleString()} files
            </span>
          )}
          {j.skipped > 0 && (
            <span title="symlinks, special files and unsafe names are not copied by SFTP">
              {j.skipped} skipped
            </span>
          )}
          {running && <span className="tabular-nums">{humanSize(Math.round(rate))}/s</span>}
          {eta !== null && <span>{seconds(eta * 1000)} left</span>}
          {j.state === "done" && j.elapsed_ms > 0 && (
            <span>avg {humanSize(Math.round((j.bytes_done * 1000) / j.elapsed_ms))}/s</span>
          )}
        </div>
        {j.error && <div className="num mt-[1px] break-all text-red-500">{j.error}</div>}
      </div>
      <div className="flex gap-2">
        {(j.state === "running" || j.state === "queued") && (
          <button
            type="button"
            onClick={() => void transferCancel(j.id)}
            className="text-text-lo hover:text-red-500"
            title="Cancel: partial files are removed, nothing lands under the real name"
          >
            cancel
          </button>
        )}
        {j.exists && (
          <button
            type="button"
            onClick={() =>
              void transferStart(j.key, j.direction, j.source, j.target_dir, true)
            }
            className="text-amber-500 hover:text-text-hi"
            title="Replace the existing target (a directory is merged, colliding files replaced)"
          >
            replace
          </button>
        )}
      </div>
    </li>
  );
}

function seconds(ms: number): string {
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)} s`;
  if (s < 120) return `${Math.round(s)} s`;
  const m = Math.floor(s / 60);
  return `${m} min ${Math.round(s - m * 60)} s`;
}
