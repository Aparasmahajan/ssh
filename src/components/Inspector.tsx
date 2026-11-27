import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { humanSize, type ConnectionInfo } from "@/lib/remote";
import { MOD_LABEL } from "@/lib/platform";
import { useSettings } from "@/state/settings";

/**
 * UI_BUILD_GUIDE §0.5 zone 4 — context for the active session.
 *
 * The guide's sketch shows RTT, CPU, MEM, cipher, key type and uptime. Only some
 * of that is knowable today, and the rest is **absent rather than invented**:
 * CPU and memory of a remote host would need an agent or a shell command run
 * behind the user's back, and neither is something to fake.
 *
 * Everything shown here is measured.
 */

/**
 * Predictive echo telemetry (CLAUDE.md §5.5).
 *
 * "Measure and expose the hit rate in the perf overlay. If prediction hit rate
 * is not instrumented, prediction is not shippable." This is that exposure.
 * `null` for a local shell, where prediction does not run.
 */
interface PredictionStat {
  hit_rate: number;
  samples: number;
  confirmed: number;
  missed: number;
  timed_out: number;
  flushes: number;
  /** Tentative predictions resolved without being drawn. Not in the hit rate. */
  withheld: number;
  outstanding: number;
  disabled: boolean;
  echo_off: boolean;
}

interface SessionStat {
  id: number;
  remote: boolean;
  bytes_in: number;
  bytes_out: number;
  rows: number;
  cols: number;
  alive: boolean;
  title: string;
  prediction: PredictionStat | null;
}

export function Inspector({ connection }: { connection: ConnectionInfo | null }) {
  const [sessions, setSessions] = useState<SessionStat[]>([]);
  const fontPx = useSettings((s) => s.terminalFontPx);

  useEffect(() => {
    const tick = () =>
      invoke<SessionStat[]>("session_stats")
        .then(setSessions)
        .catch(() => undefined);
    tick();
    const t = window.setInterval(tick, 500);
    return () => window.clearInterval(t);
  }, []);

  const totalIn = sessions.reduce((a, s) => a + s.bytes_in, 0);
  const totalOut = sessions.reduce((a, s) => a + s.bytes_out, 0);

  return (
    <aside
      data-testid="inspector"
      className="flex w-inspector shrink-0 flex-col overflow-hidden border-l-hairline
                 border-bezel-500 bg-chassis-800"
    >
      <div className="border-b-hairline border-bezel-500 px-3 py-2 font-condensed uppercase
                      text-eyebrow text-text-lo">
        inspector
      </div>

      <div className="min-h-0 flex-1 overflow-auto p-3">
        {connection ? (
          <Group title="connection">
            <Row label="host" value={`${connection.host}:${connection.port}`} />
            <Row label="user" value={connection.user} />
            <Row label="identity" value={connection.identity} />
            {/* russh 0.62 does not expose negotiated algorithms — see
                StatusRail. Saying so beats printing a plausible cipher name. */}
            <Row label="cipher" value="n/a" hint="not exposed by russh 0.62" />
          </Group>
        ) : (
          <Group title="connection">
            <p className="text-micro text-text-lo">
              Local session. A shell on this machine is a pipe, not an encrypted
              channel — there is no cipher or RTT to report.
            </p>
          </Group>
        )}

        <Group title="sessions">
          {sessions.length === 0 ? (
            <p className="text-micro text-text-lo">none open</p>
          ) : (
            sessions.map((s) => (
              <div key={s.id} className="mb-2 border-hairline border-bezel-500 p-2">
                <div className="flex items-baseline gap-2">
                  <span
                    aria-hidden
                    className={`h-[6px] w-[6px] rounded-full ${
                      !s.alive ? "bg-red-500" : s.remote ? "bg-cyan-400" : "bg-text-lo"
                    }`}
                  />
                  <span className="num truncate text-micro text-text-hi">
                    {s.remote ? "remote" : "local"} #{s.id}
                  </span>
                  <span className="num ml-auto text-micro text-text-lo">
                    {s.cols}×{s.rows}
                  </span>
                </div>
                <div className="num mt-1 flex gap-3 text-micro text-text-mid">
                  <span>↓ {humanSize(s.bytes_in)}</span>
                  <span>↑ {humanSize(s.bytes_out)}</span>
                  {!s.alive && <span className="text-red-500">ended</span>}
                </div>
                {s.prediction && <Prediction p={s.prediction} />}
              </div>
            ))
          )}
        </Group>

        <Group title="totals">
          <Row label="received" value={humanSize(totalIn)} />
          <Row label="sent" value={humanSize(totalOut)} />
          <Row label="sessions" value={String(sessions.length)} />
        </Group>

        <Group title="display">
          <Row label="font" value={`${fontPx}px`} hint={`${MOD_LABEL} + / − / 0`} />
        </Group>
      </div>
    </aside>
  );
}

/**
 * The prediction readout §5.5 requires before the feature is shippable.
 *
 * Deliberately honest about how little it knows early on: below 20 samples the
 * predictor does not act on the hit rate, so neither does this — it shows the
 * sample count instead of a percentage that would swing between 0% and 100% on
 * one keystroke.
 */
function Prediction({ p }: { p: PredictionStat }) {
  const pct = Math.round(p.hit_rate * 100);
  const judged = p.samples >= 20;
  return (
    <div className="num mt-1 flex items-baseline gap-2 text-micro">
      <span className="text-text-lo">echo</span>
      {p.disabled ? (
        // Amber is the accent for "something you should know about", and a
        // session that gave up on prediction is exactly that.
        <span className="text-amber-500" title="hit rate fell below 85% — §5.5">
          off · unpredictable link
        </span>
      ) : p.echo_off ? (
        <span className="text-text-mid" title="the server is not echoing — a password prompt">
          held · no echo
        </span>
      ) : judged ? (
        <span className={pct >= 85 ? "text-text-mid" : "text-amber-500"}>
          {pct}% of {p.samples}
        </span>
      ) : (
        <span className="text-text-lo" title="not enough samples to judge a hit rate yet">
          {p.samples} sample{p.samples === 1 ? "" : "s"}
        </span>
      )}
      {p.withheld > 0 && (
        <span
          className="text-text-lo"
          title="first keys of a line and keys at no-echo prompts: held until the server echoes, never drawn on a guess"
        >
          · {p.withheld} held
        </span>
      )}
      {p.outstanding > 0 && <span className="ml-auto text-text-lo">{p.outstanding} pending</span>}
    </div>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="mb-4">
      <p className="mb-1 font-condensed uppercase text-eyebrow text-text-lo">{title}</p>
      {children}
    </section>
  );
}

function Row({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="flex items-baseline gap-2 py-[1px]" {...(hint ? { title: hint } : {})}>
      <span className="w-[76px] shrink-0 text-micro text-text-lo">{label}</span>
      <span className="num min-w-0 flex-1 truncate text-micro text-text-mid">{value}</span>
    </div>
  );
}
