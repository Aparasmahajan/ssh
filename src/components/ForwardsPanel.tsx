import { useCallback, useEffect, useState } from "react";
import { forwardList, forwardStart, forwardStop, type ForwardStatus } from "@/lib/workspace";
import { humanSize, type ConnectionInfo } from "@/lib/remote";

/**
 * Port forwarding — the Termius "port forwarding" equivalent.
 *
 * Counters are **polled at 10 Hz**, matching CLAUDE.md §13.1 rule 8: "Progress
 * via `AtomicU64` sampled by the UI at 10 Hz. Never one message per chunk." A
 * busy tunnel moves thousands of chunks a second and pushing an event per chunk
 * would melt the frame budget.
 */
export function ForwardsPanel({ connection }: { connection: ConnectionInfo | null }) {
  const [rows, setRows] = useState<ForwardStatus[]>([]);
  const [error, setError] = useState<string | null>(null);

  const [kind, setKind] = useState<"local" | "remote" | "dynamic">("local");
  const [bindPort, setBindPort] = useState(0);
  const [targetHost, setTargetHost] = useState("");
  const [targetPort, setTargetPort] = useState(0);

  const refresh = useCallback(() => {
    forwardList().then(setRows).catch(() => undefined);
  }, []);

  useEffect(() => {
    refresh();
    const t = window.setInterval(refresh, 100); // 10 Hz
    return () => window.clearInterval(t);
  }, [refresh]);

  const start = async () => {
    if (!connection) return;
    setError(null);
    try {
      await forwardStart({
        connectionKey: connection.key,
        id: `fw-${Date.now().toString(36)}`,
        kind,
        bindAddress: "127.0.0.1",
        bindPort,
        ...(kind === "local" ? { targetHost, targetPort } : {}),
      });
      refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section className="flex min-h-0 flex-1 flex-col overflow-hidden bg-chassis-900">
      <header className="flex items-center gap-2 border-b-hairline border-bezel-500 bg-chassis-800 px-3 py-2">
        <span className="font-condensed uppercase text-eyebrow text-text-lo">port forwarding</span>
        {connection ? (
          <span className="num text-micro text-text-mid">
            over {connection.user}@{connection.host}
          </span>
        ) : (
          <span className="num text-micro text-red-500">connect to a host first</span>
        )}
      </header>

      <div className="flex flex-wrap items-end gap-2 border-b-hairline border-bezel-500 px-3 py-2">
        <Field label="type">
          <select
            value={kind}
            onChange={(e) => setKind(e.target.value as "local" | "remote" | "dynamic")}
            className="border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                       text-text-hi focus:border-amber-500 focus:outline-none"
          >
            <option value="local">local (-L)</option>
            <option value="remote">remote (-R)</option>
            <option value="dynamic">SOCKS5 (-D)</option>
          </select>
        </Field>

        <Field label={kind === "remote" ? "remote port" : "local port"}>
          <Num value={bindPort} onChange={setBindPort} />
        </Field>

        {(kind === "local" || kind === "remote") && (
          <>
            <Field label="target host">
              <input
                value={targetHost}
                onChange={(e) => setTargetHost(e.target.value)}
                placeholder="10.20.9.4"
                spellCheck={false}
                className="num w-40 border-hairline border-bezel-500 bg-chassis-900 px-2 py-1
                           text-small text-text-hi placeholder:text-text-lo
                           focus:border-amber-500 focus:outline-none"
              />
            </Field>
            <Field label="target port">
              <Num value={targetPort} onChange={setTargetPort} />
            </Field>
          </>
        )}

        <button
          type="button"
          disabled={
            !connection || (kind !== "dynamic" && (!targetHost || !targetPort))
          }
          onClick={() => void start()}
          className="border-hairline border-amber-500 bg-amber-900 px-3 py-1 text-small
                     text-amber-500 disabled:opacity-40"
        >
          Start
        </button>

        <span className="num ml-auto text-micro text-text-lo">
          {kind === "remote"
            ? "the SERVER listens; traffic arrives here"
            : "binds 127.0.0.1 — never 0.0.0.0"}
        </span>
      </div>

      {error && (
        <div className="border-b-hairline border-bezel-500 px-3 py-1 text-micro text-red-500">
          {error}
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-auto">
        <div className="grid grid-cols-[70px_1fr_1fr_70px_90px_90px_60px] gap-2 border-b-hairline border-bezel-500 px-3 py-1">
          {["type", "listening", "target", "active", "in", "out", ""].map((h, i) => (
            <span key={i} className="font-condensed uppercase text-eyebrow text-text-lo">
              {h}
            </span>
          ))}
        </div>

        {rows.map((f) => (
          <div
            key={f.id}
            className="row-hoverable grid grid-cols-[70px_1fr_1fr_70px_90px_90px_60px] items-center
                       gap-2 px-3 py-[2px] text-small"
          >
            <span className="num text-micro text-text-mid">{f.kind}</span>
            <span className="num text-text-hi" title={f.kind === "remote" ? "on the server" : "on this machine"}>
              {f.kind === "remote" ? `server:${f.bound.split(":").pop()}` : f.bound}
            </span>
            <span className="num text-text-mid">{f.target ?? "— per SOCKS request —"}</span>
            {/* Cyan is link state, and an active tunnel is exactly that (§0.2). */}
            <span className={`num ${f.active > 0 ? "text-cyan-400" : "text-text-lo"}`} data-live>
              {f.active}/{f.connections}
            </span>
            <span className="num text-micro text-text-mid" data-live>
              {humanSize(f.bytes_in)}
            </span>
            <span className="num text-micro text-text-mid" data-live>
              {humanSize(f.bytes_out)}
            </span>
            <button
              type="button"
              onClick={() => void forwardStop(f.id).then(refresh)}
              className="num text-micro text-text-lo hover:text-red-500"
            >
              stop
            </button>
          </div>
        ))}

        {rows.length === 0 && (
          <p className="p-3 text-small text-text-lo">
            No forwards running. A local forward makes a remote service reachable at
            127.0.0.1; SOCKS5 proxies everything through the host.
          </p>
        )}
      </div>
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="flex flex-col gap-1">
      <span className="font-condensed uppercase text-eyebrow text-text-lo">{label}</span>
      {children}
    </label>
  );
}

function Num({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <input
      type="number"
      value={value}
      min={0}
      max={65535}
      onChange={(e) => onChange(Number(e.target.value))}
      className="num w-24 border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                 text-text-hi focus:border-amber-500 focus:outline-none"
    />
  );
}
