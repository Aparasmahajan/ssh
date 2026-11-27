import { useEffect, useRef, useState, type ReactNode } from "react";

/**
 * Small pieces shared by the Docker, Kubernetes and Databases surfaces, so the
 * three modules read as one instrument rather than three apps.
 */

export function ModuleHeader({
  title,
  detail,
  status,
  children,
}: {
  title: string;
  /** e.g. the endpoint or context — what this is connected to. */
  detail?: ReactNode;
  status?: { tone: Tone; text: string } | null;
  /** Right-hand controls. */
  children?: ReactNode;
}) {
  return (
    <div className="flex h-tabstrip shrink-0 items-center gap-3 border-b-hairline border-bezel-500 bg-chassis-800 px-3">
      <span className="font-condensed uppercase text-eyebrow text-text-hi">{title}</span>
      {status && <StatusText tone={status.tone}>{status.text}</StatusText>}
      {detail && <span className="num min-w-0 truncate text-micro text-text-lo">{detail}</span>}
      <span className="ml-auto flex shrink-0 items-center gap-2">{children}</span>
    </div>
  );
}

export type Tone = "live" | "warn" | "bad" | "idle" | "info";

const DOT: Record<Tone, string> = {
  live: "bg-cyan-400",
  warn: "bg-amber-500",
  bad: "bg-red-500",
  idle: "bg-text-lo",
  info: "bg-violet-400",
};
const TEXT: Record<Tone, string> = {
  live: "text-cyan-400",
  warn: "text-amber-500",
  bad: "text-red-500",
  idle: "text-text-lo",
  info: "text-violet-400",
};

export function Dot({ tone }: { tone: Tone }) {
  return <span aria-hidden className={`inline-block h-[6px] w-[6px] shrink-0 rounded-full ${DOT[tone]}`} />;
}

export function StatusText({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span className={`num flex items-center gap-1 whitespace-nowrap text-micro ${TEXT[tone]}`}>
      <Dot tone={tone} />
      {children}
    </span>
  );
}

export function ToolButton({
  children,
  onClick,
  disabled,
  danger,
  title,
}: {
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
  title?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      className={`num rounded border-hairline border-bezel-500 px-[6px] py-[1px] text-micro text-text-mid
                  disabled:cursor-not-allowed disabled:opacity-40
                  ${danger ? "hover:border-red-500 hover:text-red-500" : "hover:border-amber-500 hover:text-amber-500"}`}
    >
      {children}
    </button>
  );
}

/**
 * A destructive action that needs a second click within 3 s. Inline rather than
 * a modal: §0.8 is keyboard-first, and a dialog for "remove this container" is
 * friction for the common case while still stopping the accidental one.
 */
export function ConfirmButton({
  children,
  confirmLabel,
  onConfirm,
  disabled,
  target,
}: {
  children: ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  disabled?: boolean;
  /** What would be destroyed. Arming is for THIS target only: selecting a
   *  different row disarms, or "remove?" armed on container A would remove
   *  container B with a single click. */
  target: string | null;
}) {
  const [armed, setArmed] = useState(false);
  const timer = useRef<number | null>(null);
  const clear = () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = null;
  };
  useEffect(() => clear, []);
  useEffect(() => {
    clear();
    setArmed(false);
  }, [target]);
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={() => {
        clear();
        if (armed) {
          setArmed(false);
          onConfirm();
          return;
        }
        setArmed(true);
        timer.current = window.setTimeout(() => setArmed(false), 3000);
      }}
      className={`num rounded border-hairline px-[6px] py-[1px] text-micro disabled:cursor-not-allowed disabled:opacity-40
                  ${armed ? "border-red-500 bg-red-500 text-chassis-900" : "border-bezel-500 text-text-mid hover:border-red-500 hover:text-red-500"}`}
    >
      {armed ? confirmLabel : children}
    </button>
  );
}

export function FilterInput({
  value,
  onChange,
  placeholder = "filter",
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <input
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Escape" && value !== "") {
          onChange("");
          e.stopPropagation();
        }
      }}
      placeholder={placeholder}
      spellCheck={false}
      className="num w-48 rounded border-hairline border-bezel-500 bg-chassis-900 px-2 py-[1px] text-micro
                 text-text-hi placeholder:text-text-lo focus:border-amber-500 focus:outline-none"
    />
  );
}

/** A full-pane message: not connected, no permission, nothing to show. */
export function PaneMessage({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="flex flex-1 items-start justify-center p-8">
      <div className="max-w-xl border-hairline border-bezel-500 bg-chassis-800 p-4">
        <p className="mb-2 text-base text-text-hi">{title}</p>
        <div className="text-small text-text-mid">{children}</div>
      </div>
    </div>
  );
}

export function ErrorStrip({ error, onDismiss }: { error: string | null; onDismiss: () => void }) {
  if (!error) return null;
  return (
    <div className="num flex items-start gap-2 border-b-hairline border-bezel-500 bg-chassis-800 px-3 py-1 text-micro text-red-500">
      <span className="min-w-0 flex-1 whitespace-pre-wrap break-words">{error}</span>
      <button type="button" onClick={onDismiss} className="text-text-lo hover:text-text-hi" aria-label="Dismiss">
        ×
      </button>
    </div>
  );
}

/** Poll `fetch(generation)` every `ms`, keeping the latest non-null result.
 *  §13.1 rule 8: the UI samples state by generation; nothing is pushed. */
export function useGenerationPoll<T extends { generation: number }>(
  fetch: (since: number) => Promise<T | null>,
  ms: number,
  enabled = true,
): { data: T | null; error: string | null } {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 0 = "send everything"; live generations start at 1.
  const gen = useRef(0);
  const lastFetch = useRef(fetch);
  useEffect(() => {
    // New source (context, kind, namespace…): the previous one's data must not
    // stay on screen looking live while the new one loads — actions taken on a
    // stale row would go to the NEW target.
    // Only on a new source. Pausing and resuming (a hidden panel) keeps the
    // data, and the first sample after resuming asks for everything anyway.
    if (lastFetch.current !== fetch) {
      lastFetch.current = fetch;
      setData(null);
      setError(null);
    }
    gen.current = 0;
    if (!enabled) return;
    let alive = true;
    let busy = false;
    const tick = () => {
      if (busy) return;
      busy = true;
      fetch(gen.current)
        .then((v) => {
          if (!alive) return;
          setError(null);
          if (v) {
            gen.current = v.generation;
            setData(v);
          }
        })
        .catch((e: unknown) => alive && setError(String(e)))
        .finally(() => {
          busy = false;
        });
    };
    tick();
    const t = window.setInterval(tick, ms);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [fetch, ms, enabled]);
  return { data, error };
}
