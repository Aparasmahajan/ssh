import { useCallback, useEffect, useMemo, useState } from "react";
import {
  age,
  kubeAction,
  kubeContexts,
  kubeSnapshot,
  kubeYaml,
  type KubeContexts,
  type KubeKind,
  type KubeRow,
} from "@/lib/kube";
import { useSessions } from "@/state/sessions";
import { useLayout } from "@/state/layout";
import { DataTable, sortRows, type Column, type Sort } from "./DataTable";
import {
  ConfirmButton,
  Dot,
  ErrorStrip,
  FilterInput,
  ModuleHeader,
  PaneMessage,
  ToolButton,
  useGenerationPoll,
} from "./ModuleKit";

/**
 * Kubernetes module.
 *
 * Every list is an informer in Rust — watch, not poll (CLAUDE.md §10) — and
 * this samples it by generation. A pod restarting in another terminal shows
 * up here within one sample, and an idle cluster costs one atomic load per
 * sample.
 */

const POLL_MS = 250;
const KINDS: { id: KubeKind; label: string }[] = [
  { id: "pods", label: "Pods" },
  { id: "deployments", label: "Deployments" },
  { id: "services", label: "Services" },
  { id: "nodes", label: "Nodes" },
];
const CLUSTER_SCOPED: KubeKind[] = ["nodes", "namespaces"];

export function KubernetesPanel({ active = true }: { active?: boolean }) {
  const [ctxs, setCtxs] = useState<KubeContexts | null>(null);
  const [ctxError, setCtxError] = useState<string | null>(null);
  const [context, setContext] = useState<string>("");
  const [namespace, setNamespace] = useState<string>(""); // "" = all
  const [kind, setKind] = useState<KubeKind>("pods");
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<Sort | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [container, setContainer] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [yaml, setYaml] = useState<{ title: string; text: string } | null>(null);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));

  const openStream = useSessions((s) => s.openStream);
  const setModule = useLayout((s) => s.setActiveModule);

  useEffect(() => {
    kubeContexts()
      .then((c) => {
        setCtxs(c);
        const start = c.current ?? c.contexts[0]?.name ?? "";
        setContext(start);
        setNamespace(c.contexts.find((x) => x.name === start)?.namespace ?? "");
      })
      .catch((e: unknown) => setCtxError(String(e)));
  }, []);

  // AGE ticks without an event: re-render once a second.
  useEffect(() => {
    if (!active) return; // nothing to tick while hidden
    const t = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => window.clearInterval(t);
  }, [active]);

  const scopedNs = CLUSTER_SCOPED.includes(kind) ? "" : namespace;
  const fetchList = useCallback(
    (since: number) => kubeSnapshot(context, kind, scopedNs, since),
    [context, kind, scopedNs],
  );
  // Paused while hidden: no IPC, and Rust drops the informers after a minute.
  const { data, error: pollError } = useGenerationPoll(fetchList, POLL_MS, active && context !== "");

  const fetchNs = useCallback((since: number) => kubeSnapshot(context, "namespaces", "", since), [context]);
  const { data: nsData } = useGenerationPoll(fetchNs, 1000, active && context !== "");
  const namespaces = useMemo(() => (nsData?.rows ?? []).map((r) => r.name).sort(), [nsData]);

  const allNs = scopedNs === "" && !CLUSTER_SCOPED.includes(kind);
  // Columns WITHOUT age: filtering and sorting depend on these only, so the
  // once-a-second age tick re-renders the visible cells, not a re-filter and
  // re-sort of every row (5,000 pods, against a 16 ms repaint budget).
  const baseColumns: Column<KubeRow>[] = useMemo(() => {
    const cols: Column<KubeRow>[] = [
      {
        id: "name",
        header: "name",
        width: 300,
        text: (r) => r.name,
        render: (r) => (
          <span className="flex items-center gap-2">
            <Dot tone={r.tone} />
            <span className="truncate text-text-hi">{r.name}</span>
          </span>
        ),
      },
    ];
    if (allNs) cols.push({ id: "ns", header: "namespace", width: 150, text: (r) => r.namespace });
    (data?.columns ?? []).forEach((h, i) => {
      const numeric = h === "restarts" || h === "up-to-date" || h === "available";
      cols.push({
        id: `c${i}`,
        header: h,
        width: h === "images" || h === "ports" ? 260 : numeric ? 90 : 130,
        align: numeric ? "right" : "left",
        text: (r) => r.cells[i] ?? "",
      });
    });
    return cols;
  }, [allNs, data?.columns]);

  const columns: Column<KubeRow>[] = useMemo(
    () => [
      ...baseColumns,
      {
        id: "age",
        header: "age",
        width: 70,
        align: "right" as const,
        // Sorted by creation time (below); displayed as age.
        text: (r: KubeRow) => age(r.created, now),
      },
    ],
    [baseColumns, now],
  );

  const rows = useMemo(() => {
    const q = filter.trim().toLowerCase();
    const list = (data?.rows ?? []).filter(
      (r) =>
        q === "" ||
        r.name.toLowerCase().includes(q) ||
        r.namespace.toLowerCase().includes(q) ||
        r.cells.some((c) => c.toLowerCase().includes(q)),
    );
    if (sort?.column === "age") {
      const s = [...list].sort((a, b) => b.created - a.created);
      return sort.dir === "desc" ? s.reverse() : s;
    }
    return sortRows(list, baseColumns, sort);
  }, [baseColumns, data, filter, sort]);

  const current = rows.find((r) => r.uid === selected) ?? null;
  useEffect(() => setContainer(current?.containers[0] ?? null), [current?.uid]); // eslint-disable-line react-hooks/exhaustive-deps

  const act = async (label: string, f: () => Promise<void>) => {
    setBusy(label);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(`${label}: ${String(e)}`);
    } finally {
      setBusy(null);
    }
  };

  const shell = (r: KubeRow) => {
    openStream({ kind: "k8s-exec", context, namespace: r.namespace, pod: r.name, container });
    setModule("workspace");
  };
  const logs = (r: KubeRow) => {
    openStream({ kind: "k8s-logs", context, namespace: r.namespace, pod: r.name, container });
    setModule("workspace");
  };
  const showYaml = (r: KubeRow) =>
    act("yaml", async () => {
      const text = await kubeYaml(context, kind, r.namespace, r.name);
      setYaml({ title: `${kind.replace(/s$/, "")}/${r.name}`, text });
    });

  if (ctxError) {
    return (
      <section className="flex min-w-0 flex-1 flex-col bg-chassis-900">
        <ModuleHeader title="Kubernetes" status={{ tone: "bad", text: "no kubeconfig" }} />
        <PaneMessage title="No usable kubeconfig">
          <p className="num mb-2 text-red-500">{ctxError}</p>
          <p>
            OpsPilot reads kubeconfig the way kubectl does: <span className="num">$KUBECONFIG</span>, then{" "}
            <span className="num">~/.kube/config</span>.
          </p>
        </PaneMessage>
      </section>
    );
  }

  return (
    <section className="flex min-w-0 flex-1 flex-col overflow-hidden bg-chassis-900" data-testid="kubernetes-panel">
      <ModuleHeader
        title="Kubernetes"
        status={
          !data
            ? { tone: "idle", text: "connecting" }
            : data.error
              ? { tone: "bad", text: "error" }
              : data.synced
                ? { tone: "live", text: `watching · ${data.mode}` }
                : { tone: "warn", text: "syncing" }
        }
        detail={ctxs ? `kubeconfig ${ctxs.source}` : undefined}
      >
        <select
          aria-label="Context"
          value={context}
          onChange={(e) => {
            setContext(e.target.value);
            setNamespace(ctxs?.contexts.find((x) => x.name === e.target.value)?.namespace ?? "");
            setSelected(null);
          }}
          className="num max-w-[220px] rounded border-hairline border-bezel-500 bg-chassis-900 px-1 py-[1px] text-micro text-text-hi"
        >
          {ctxs?.contexts.map((c) => (
            <option key={c.name} value={c.name}>
              {c.name}
            </option>
          ))}
        </select>
        <select
          aria-label="Namespace"
          value={namespace}
          disabled={CLUSTER_SCOPED.includes(kind)}
          onChange={(e) => {
            setNamespace(e.target.value);
            setSelected(null);
          }}
          className="num max-w-[180px] rounded border-hairline border-bezel-500 bg-chassis-900 px-1 py-[1px] text-micro text-text-hi disabled:opacity-40"
        >
          <option value="">all namespaces</option>
          {namespace && !namespaces.includes(namespace) && <option value={namespace}>{namespace}</option>}
          {namespaces.map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
        <FilterInput value={filter} onChange={setFilter} placeholder="filter" />
      </ModuleHeader>

      <div className="flex h-[30px] shrink-0 items-center gap-1 border-b-hairline border-bezel-500 px-3">
        {KINDS.map((k) => (
          <button
            key={k.id}
            type="button"
            onClick={() => {
              setKind(k.id);
              setSelected(null);
              setSort(null);
            }}
            className={`px-2 py-[2px] font-condensed uppercase text-eyebrow ${
              kind === k.id ? "border-b-2 border-amber-500 text-text-hi" : "text-text-lo hover:text-text-hi"
            }`}
          >
            {k.label}
          </button>
        ))}
        <span className="num ml-3 text-micro text-text-lo">{data ? `${rows.length} shown` : ""}</span>

        <span className="ml-auto flex items-center gap-1">
          {current && <span className="num mr-1 max-w-[220px] truncate text-micro text-text-hi">{current.name}</span>}
          {kind === "pods" && (
            <>
              {current && current.containers.length > 1 && (
                <select
                  aria-label="Container"
                  value={container ?? ""}
                  onChange={(e) => setContainer(e.target.value)}
                  className="num rounded border-hairline border-bezel-500 bg-chassis-900 px-1 text-micro text-text-hi"
                >
                  {current.containers.map((c) => (
                    <option key={c}>{c}</option>
                  ))}
                </select>
              )}
              <ToolButton disabled={!current || current.cells[1] !== "Running"} onClick={() => current && shell(current)}>
                ⌁ shell
              </ToolButton>
              <ToolButton disabled={!current} onClick={() => current && logs(current)}>
                logs
              </ToolButton>
              <ConfirmButton
                target={current ? `${context}/${current.uid}` : null}
                disabled={!current || !!busy}
                confirmLabel="delete pod?"
                onConfirm={() =>
                  current && void act("delete pod", () => kubeAction(context, "delete-pod", current.namespace, current.name))
                }
              >
                delete
              </ConfirmButton>
            </>
          )}
          {kind === "deployments" && current && (
            <>
              <span className="num text-micro text-text-lo">replicas</span>
              <ToolButton
                disabled={!!busy || (current.replicas ?? 0) <= 0}
                onClick={() =>
                  void act("scale", () =>
                    kubeAction(context, "scale", current.namespace, current.name, (current.replicas ?? 1) - 1),
                  )
                }
              >
                −
              </ToolButton>
              <span className="num w-6 text-center text-micro text-text-hi">{current.replicas ?? "?"}</span>
              <ToolButton
                disabled={!!busy}
                onClick={() =>
                  void act("scale", () =>
                    kubeAction(context, "scale", current.namespace, current.name, (current.replicas ?? 0) + 1),
                  )
                }
              >
                +
              </ToolButton>
              <ToolButton
                disabled={!!busy}
                onClick={() =>
                  void act("rollout restart", () => kubeAction(context, "rollout-restart", current.namespace, current.name))
                }
              >
                restart
              </ToolButton>
            </>
          )}
          <ToolButton disabled={!current || !!busy} onClick={() => current && void showYaml(current)}>
            yaml
          </ToolButton>
          {busy && <span className="num ml-1 text-micro text-amber-500">{busy}…</span>}
        </span>
      </div>

      <ErrorStrip error={error ?? pollError ?? data?.error ?? null} onDismiss={() => setError(null)} />

      <div className="flex min-h-0 flex-1">
        <DataTable
          label={kind}
          columns={columns}
          rowCount={rows.length}
          rowAt={(i) => rows[i]}
          rowKey={(r) => r.uid}
          sort={sort}
          onSort={setSort}
          selectedKey={selected}
          onSelect={(r) => setSelected(r.uid)}
          onActivate={(r) => (kind === "pods" ? (r.cells[1] === "Running" ? shell(r) : logs(r)) : void showYaml(r))}
          empty={!data ? "connecting…" : data.synced ? "none" : "syncing…"}
        />
        {yaml && (
          <aside className="flex w-[46%] min-w-[360px] flex-col border-l-hairline border-bezel-500 bg-chassis-800">
            <div className="flex h-[28px] shrink-0 items-center border-b-hairline border-bezel-500 px-3">
              <span className="num truncate text-micro text-text-hi">{yaml.title}</span>
              <button
                type="button"
                onClick={() => void navigator.clipboard?.writeText(yaml.text)}
                className="num ml-auto text-micro text-text-lo hover:text-amber-500"
              >
                copy
              </button>
              <button
                type="button"
                aria-label="Close YAML"
                onClick={() => setYaml(null)}
                className="ml-3 text-small text-text-lo hover:text-text-hi"
              >
                ×
              </button>
            </div>
            <pre className="selectable min-h-0 flex-1 overflow-auto p-3 font-[family-name:var(--font-term)] text-micro text-text-mid">
              {yaml.text}
            </pre>
          </aside>
        )}
      </div>
    </section>
  );
}
