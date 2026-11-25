import { useCallback, useMemo, useState } from "react";
import { dockerAction, dockerSnapshot, type Container, type DockerAction } from "@/lib/docker";
import { humanSize } from "@/lib/remote";
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
  type Tone,
} from "./ModuleKit";

/**
 * Docker module — containers on the local (or `DOCKER_HOST`) daemon.
 *
 * Live by construction: the Rust side follows the daemon's event stream and
 * per-container stats streams (CLAUDE.md §10), and this samples the result by
 * generation four times a second. A container started from a terminal appears
 * here without a refresh button, because there is no refresh button.
 */

const POLL_MS = 250;

function tone(state: string): Tone {
  switch (state) {
    case "running":
      return "live";
    case "paused":
    case "restarting":
    case "removing":
      return "warn";
    case "dead":
      return "bad";
    case "created":
      return "info";
    default:
      return "idle";
  }
}

const COLUMNS: Column<Container>[] = [
  {
    id: "name",
    header: "name",
    width: 240,
    text: (c) => (c.project ? `${c.project} / ${c.name}` : c.name),
    render: (c) => (
      <span className="flex items-center gap-2">
        <Dot tone={tone(c.state)} />
        <span className="truncate text-text-hi">{c.name}</span>
      </span>
    ),
  },
  { id: "project", header: "project", width: 120, text: (c) => c.project ?? "" },
  { id: "image", header: "image", width: 220, text: (c) => c.image },
  { id: "status", header: "status", width: 180, text: (c) => c.status },
  {
    id: "cpu",
    header: "cpu",
    width: 70,
    align: "right",
    // Sorted numerically by the collator, so the text must be the number.
    text: (c) => (c.cpu_pct === null ? "" : `${c.cpu_pct.toFixed(1)}%`),
  },
  {
    id: "mem",
    header: "memory",
    width: 90,
    align: "right",
    text: (c) => (c.mem_bytes === null ? "" : humanSize(c.mem_bytes)),
  },
  { id: "ports", header: "ports", width: 260, text: (c) => c.ports },
  { id: "id", header: "id", width: 110, text: (c) => c.id.slice(0, 12) },
];

export function DockerPanel({ active = true }: { active?: boolean }) {
  // Paused while hidden: no IPC, and the Rust side drops the watch after a
  // minute unsampled.
  const { data, error: pollError } = useGenerationPoll(dockerSnapshot, POLL_MS, active);
  const [filter, setFilter] = useState("");
  const [showStopped, setShowStopped] = useState(true);
  const [sort, setSort] = useState<Sort | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const openStream = useSessions((s) => s.openStream);
  const setModule = useLayout((s) => s.setActiveModule);

  const rows = useMemo(() => {
    const q = filter.trim().toLowerCase();
    const list = (data?.containers ?? []).filter(
      (c) =>
        (showStopped || c.state === "running" || c.state === "paused" || c.state === "restarting") &&
        (q === "" ||
          c.name.toLowerCase().includes(q) ||
          c.image.toLowerCase().includes(q) ||
          (c.project ?? "").toLowerCase().includes(q) ||
          c.id.startsWith(q)),
    );
    return sortRows(list, COLUMNS, sort);
  }, [data, filter, showStopped, sort]);

  const current = rows.find((c) => c.id === selected) ?? null;
  const running = current?.state === "running";

  const run = useCallback(async (c: Container, action: DockerAction) => {
    setBusy(`${action} ${c.name}`);
    setError(null);
    try {
      await dockerAction(c.id, action);
    } catch (e) {
      setError(`${action} ${c.name}: ${String(e)}`);
    } finally {
      setBusy(null);
    }
  }, []);

  const shell = useCallback(
    (c: Container) => {
      openStream({ kind: "docker-exec", id: c.id, name: c.name });
      // Tabs live in the workspace surface; take the user to the new one.
      setModule("workspace");
    },
    [openStream, setModule],
  );
  const logs = useCallback(
    (c: Container) => {
      openStream({ kind: "docker-logs", id: c.id, name: c.name });
      setModule("workspace");
    },
    [openStream, setModule],
  );

  const total = data?.containers.length ?? 0;
  const live = data?.containers.filter((c) => c.state === "running").length ?? 0;

  return (
    <section className="flex min-w-0 flex-1 flex-col overflow-hidden bg-chassis-900" data-testid="docker-panel">
      <ModuleHeader
        title="Docker"
        status={
          data === null
            ? { tone: "idle", text: "connecting" }
            : data.connected
              ? { tone: "live", text: `engine ${data.version ?? ""}` }
              : { tone: "bad", text: "not connected" }
        }
        detail={data?.endpoint ? `${data.endpoint} · ${data.source ?? ""}` : undefined}
      >
        <span className="num text-micro text-text-lo">
          {live} running · {total} total
        </span>
        <label className="num flex items-center gap-1 text-micro text-text-mid">
          <input type="checkbox" checked={showStopped} onChange={(e) => setShowStopped(e.target.checked)} />
          stopped
        </label>
        <FilterInput value={filter} onChange={setFilter} placeholder="filter name, image, project" />
      </ModuleHeader>

      {/* Actions for the selected container. Disabled rather than hidden, so
          the toolbar does not jump around as the selection changes. */}
      <div className="flex h-[30px] shrink-0 items-center gap-1 border-b-hairline border-bezel-500 px-3">
        <span className="num mr-2 min-w-0 max-w-[260px] truncate text-micro text-text-hi">
          {current ? current.name : <span className="text-text-lo">select a container</span>}
        </span>
        <ToolButton disabled={!running} onClick={() => current && shell(current)} title="Interactive shell (Enter)">
          ⌁ shell
        </ToolButton>
        <ToolButton disabled={!current} onClick={() => current && logs(current)}>
          logs
        </ToolButton>
        <span className="mx-1 h-4 w-px bg-bezel-500" />
        {current && (current.state === "running" || current.state === "restarting") ? (
          <ToolButton disabled={!!busy} onClick={() => run(current, "stop")}>
            stop
          </ToolButton>
        ) : (
          <ToolButton disabled={!current || !!busy || current.state === "paused"} onClick={() => current && run(current, "start")}>
            start
          </ToolButton>
        )}
        <ToolButton disabled={!current || !!busy} onClick={() => current && run(current, "restart")}>
          restart
        </ToolButton>
        {current?.state === "paused" ? (
          <ToolButton disabled={!!busy} onClick={() => run(current, "unpause")}>
            unpause
          </ToolButton>
        ) : (
          <ToolButton disabled={!running || !!busy} onClick={() => current && run(current, "pause")}>
            pause
          </ToolButton>
        )}
        <ConfirmButton
          target={current?.id ?? null}
          disabled={!current || !!busy}
          confirmLabel={running ? "force remove?" : "remove?"}
          onConfirm={() => current && run(current, running ? "remove-force" : "remove")}
        >
          remove
        </ConfirmButton>
        {busy && <span className="num ml-2 text-micro text-amber-500">{busy}…</span>}
      </div>

      <ErrorStrip error={error ?? pollError} onDismiss={() => setError(null)} />

      {data && !data.connected ? (
        <PaneMessage title="Docker is not reachable">
          <p className="num mb-2 text-red-500">{data.error}</p>
          <p>
            Looked at <span className="num text-text-hi">{data.endpoint ?? "?"}</span>
            {data.source ? ` (${data.source})` : ""}. Start Docker (or Docker Desktop) and this
            connects by itself — it retries in the background.
          </p>
          <p className="mt-2 text-text-lo">
            The endpoint is resolved like the docker CLI: DOCKER_HOST, then the current docker
            context, then the default socket. On Linux, permission errors mean your user is not in
            the <span className="num">docker</span> group.
          </p>
        </PaneMessage>
      ) : (
        <DataTable
          label="Containers"
          columns={COLUMNS}
          rowCount={rows.length}
          rowAt={(i) => rows[i]}
          rowKey={(c) => c.id}
          sort={sort}
          onSort={setSort}
          selectedKey={selected}
          onSelect={(c) => setSelected(c.id)}
          onActivate={(c) => (c.state === "running" ? shell(c) : logs(c))}
          empty={data === null ? "connecting…" : filter ? "no containers match" : "no containers"}
        />
      )}
    </section>
  );
}
