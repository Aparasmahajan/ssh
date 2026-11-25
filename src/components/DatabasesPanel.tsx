import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  dbCancel,
  dbClose,
  dbOpen,
  dbProfileDelete,
  dbProfileSave,
  dbProfiles,
  dbRun,
  dbSetView,
  dbWindow,
  type DbOpened,
  type DbProfile,
  type DbWindow,
} from "@/lib/db";
import { DataTable, ROW_H, type Column, type Sort } from "./DataTable";
import { ConfirmButton, Dot, ErrorStrip, FilterInput, ModuleHeader, PaneMessage, ToolButton } from "./ModuleKit";

/**
 * Databases module — Postgres, MySQL/MariaDB, SQLite.
 *
 * The result grid never holds the result. Rust keeps it in Arrow columns and
 * this asks for the window being painted (CLAUDE.md §11); scrolling to the
 * bottom of what is loaded is what makes the engine fetch the next 500 rows.
 * Sorting and filtering run in Rust over the whole result.
 */

const POLL_MS = 200;
const ENGINES: DbProfile["engine"][] = ["postgres", "mysql", "sqlite"];
const DEFAULT_PORT: Record<DbProfile["engine"], number> = { postgres: 5432, mysql: 3306, sqlite: 0 };

type Row = { index: number; cells: (string | null)[] };

export function DatabasesPanel({ active = true }: { active?: boolean }) {
  const [profiles, setProfiles] = useState<DbProfile[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<DbProfile | null>(null);
  const [open, setOpen] = useState<DbOpened | null>(null);
  const [password, setPassword] = useState("");
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    dbProfiles().then(setProfiles).catch((e: unknown) => setError(String(e)));
  }, []);

  const selected = profiles.find((p) => p.id === selectedId) ?? null;
  const needsPassword = selected !== null && selected.engine !== "sqlite" && !selected.password_ref;

  // Close the session when the panel goes away, or the connection leaks.
  const openRef = useRef<DbOpened | null>(null);
  openRef.current = open;
  const mounted = useRef(true);
  /** Bumped by every connect and disconnect; an open that resolves under an
   *  older value was superseded and is closed instead of adopted. */
  const attempt = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      attempt.current += 1;
      if (openRef.current) void dbClose(openRef.current.session).catch(() => undefined);
    };
  }, []);

  const connect = async () => {
    // A second Enter while connecting must not open a second session.
    if (!selected || connecting) return;
    const mine = ++attempt.current;
    setConnecting(true);
    setError(null);
    try {
      const prev = openRef.current;
      if (prev) {
        // Forget it immediately: if the new open fails or is slow, the UI must
        // not sit on a session id that no longer exists.
        setOpen(null);
        openRef.current = null;
        await dbClose(prev.session).catch(() => undefined);
      }
      const o = await dbOpen(selected.id, needsPassword ? password : null);
      if (!mounted.current || mine !== attempt.current) {
        // Unmounted or superseded while connecting: nobody owns this one.
        void dbClose(o.session).catch(() => undefined);
        return;
      }
      setOpen(o);
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) {
        setConnecting(false);
        // The typed password is for this one connect; do not keep it around.
        setPassword("");
      }
    }
  };

  const disconnect = async () => {
    attempt.current += 1;
    const prev = openRef.current;
    setOpen(null);
    openRef.current = null;
    if (prev) await dbClose(prev.session).catch(() => undefined);
  };

  return (
    <section className="flex min-w-0 flex-1 overflow-hidden bg-chassis-900" data-testid="databases-panel">
      {/* Saved connections */}
      <aside className="flex w-[240px] shrink-0 flex-col border-r-hairline border-bezel-500 bg-chassis-800">
        <div className="flex h-tabstrip shrink-0 items-center border-b-hairline border-bezel-500 px-3">
          <span className="font-condensed uppercase text-eyebrow text-text-hi">Databases</span>
          <button
            type="button"
            onClick={() => setEditing(blank())}
            className="ml-auto text-small text-text-lo hover:text-amber-500"
            aria-label="New connection"
          >
            +
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-auto py-1">
          {profiles.length === 0 && (
            <p className="px-3 py-2 text-small text-text-lo">
              No saved connections. They are stored in <span className="num">databases.toml</span>{" "}
              — passwords only as vault references.
            </p>
          )}
          {profiles.map((p) => {
            const isOpen = open !== null && selectedId === p.id;
            return (
              <button
                key={p.id}
                type="button"
                onClick={() => setSelectedId(p.id)}
                onDoubleClick={() => setEditing(p)}
                className={`row-hoverable flex w-full items-center gap-2 px-3 py-1 text-left text-small
                            ${selectedId === p.id ? "bg-chassis-700 text-text-hi" : "text-text-mid"}`}
              >
                <Dot tone={isOpen ? "live" : "idle"} />
                <span className="min-w-0 flex-1 truncate">{p.name}</span>
                <span className="num text-micro text-text-lo">{p.engine}</span>
              </button>
            );
          })}
        </div>
        {selected && (
          <div className="shrink-0 space-y-2 border-t-hairline border-bezel-500 p-3">
            <p className="num truncate text-micro text-text-lo" title={describe(selected)}>
              {describe(selected)}
            </p>
            {plaintextRemote(selected) && (
              // D-30: no TLS yet. Loud only where it matters — a remote host.
              <p className="text-micro text-amber-500">
                Unencrypted connection to a remote host. Reach it through an SSH tunnel (Transfers) instead.
              </p>
            )}
            {needsPassword && (
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && void connect()}
                placeholder="password (not saved)"
                autoComplete="off"
                className="num w-full rounded border-hairline border-bezel-500 bg-chassis-900 px-2 py-[2px]
                           text-micro text-text-hi placeholder:text-text-lo focus:border-amber-500 focus:outline-none"
              />
            )}
            <div className="flex flex-wrap gap-1">
              <ToolButton onClick={() => void connect()} disabled={connecting}>
                {connecting ? "connecting…" : open ? "reconnect" : "connect"}
              </ToolButton>
              <ToolButton onClick={() => setEditing(selected)}>edit</ToolButton>
              <ConfirmButton
                target={selected.id}
                confirmLabel="delete?"
                onConfirm={() =>
                  void dbProfileDelete(selected.id)
                    .then((all) => {
                      setProfiles(all);
                      setSelectedId(null);
                    })
                    .catch((e: unknown) => setError(String(e)))
                }
              >
                delete
              </ConfirmButton>
            </div>
          </div>
        )}
      </aside>

      <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
        <ErrorStrip error={error} onDismiss={() => setError(null)} />
        {editing ? (
          <ProfileEditor
            initial={editing}
            onCancel={() => setEditing(null)}
            onSave={async (p) => {
              try {
                const all = await dbProfileSave(p);
                setProfiles(all);
                setSelectedId(p.id);
                setEditing(null);
              } catch (e) {
                setError(String(e));
              }
            }}
          />
        ) : open ? (
          <QueryWorkspace
            key={open.session}
            opened={open}
            active={active}
            onDisconnect={() => void disconnect()}
          />
        ) : (
          <PaneMessage title="Not connected">
            <p>Pick a saved connection on the left and connect, or add one with +.</p>
            <p className="mt-2 text-text-lo">
              Postgres results stream through a server-side cursor 500 rows at a time; MySQL and
              SQLite fetch the same way. A million-row query costs one chunk until you scroll.
            </p>
          </PaneMessage>
        )}
      </div>
    </section>
  );
}

function blank(): DbProfile {
  return { id: "", name: "", engine: "postgres", host: "127.0.0.1", port: 5432, user: "", database: "" };
}

/** No TLS yet (docs/DECISIONS.md D-30): flag anything that is not loopback. */
function plaintextRemote(p: DbProfile): boolean {
  if (p.engine === "sqlite") return false;
  const h = (p.host ?? "").trim().toLowerCase();
  return !(h === "" || h === "localhost" || h === "::1" || h.startsWith("127."));
}

function describe(p: DbProfile): string {
  if (p.engine === "sqlite") return p.path ?? "";
  return `${p.user ?? ""}@${p.host ?? ""}:${p.port || DEFAULT_PORT[p.engine]}/${p.database ?? ""}`;
}

function slug(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
}

/* ---- Profile editor ------------------------------------------------------------ */

function ProfileEditor({
  initial,
  onSave,
  onCancel,
}: {
  initial: DbProfile;
  onSave: (p: DbProfile) => void | Promise<void>;
  onCancel: () => void;
}) {
  const [p, setP] = useState<DbProfile>(initial);
  const isNew = initial.id === "";
  const set = <K extends keyof DbProfile>(k: K, v: DbProfile[K]) => setP((x) => ({ ...x, [k]: v }));

  const field = (label: string, input: React.ReactNode, hint?: string) => (
    <label className="grid grid-cols-[120px_1fr] items-center gap-3 py-1">
      <span className="text-small text-text-mid">{label}</span>
      <span className="flex flex-col">
        {input}
        {hint && <span className="mt-[2px] text-micro text-text-lo">{hint}</span>}
      </span>
    </label>
  );
  const input = (k: "name" | "host" | "user" | "database" | "password_ref" | "path", placeholder = "") => (
    <input
      value={(p[k] as string | null | undefined) ?? ""}
      onChange={(e) => set(k, e.target.value)}
      placeholder={placeholder}
      spellCheck={false}
      className="num rounded border-hairline border-bezel-500 bg-chassis-900 px-2 py-[3px] text-small
                 text-text-hi placeholder:text-text-lo focus:border-amber-500 focus:outline-none"
    />
  );

  return (
    <form
      className="max-w-2xl overflow-auto p-5"
      onSubmit={(e) => {
        e.preventDefault();
        const id = isNew ? slug(p.name) : p.id;
        void onSave({ ...p, id });
      }}
    >
      <h2 className="mb-3 font-condensed uppercase text-eyebrow text-text-hi">
        {isNew ? "New connection" : `Edit ${initial.name}`}
      </h2>
      {field("name", input("name", "orders (prod)"))}
      {field(
        "engine",
        <select
          value={p.engine}
          onChange={(e) => {
            const engine = e.target.value as DbProfile["engine"];
            setP((x) => ({ ...x, engine, port: DEFAULT_PORT[engine] }));
          }}
          className="num rounded border-hairline border-bezel-500 bg-chassis-900 px-2 py-[3px] text-small text-text-hi"
        >
          {ENGINES.map((e) => (
            <option key={e} value={e}>
              {e}
            </option>
          ))}
        </select>,
      )}
      {p.engine === "sqlite" ? (
        field("file", input("path", "~/data/app.db"), "Opened read-write; never created if missing. Stored as ~/… so the config stays portable.")
      ) : (
        <>
          {field("host", input("host", "127.0.0.1"))}
          {field(
            "port",
            <input
              type="number"
              value={p.port ?? DEFAULT_PORT[p.engine]}
              onChange={(e) => set("port", Number(e.target.value))}
              className="num w-28 rounded border-hairline border-bezel-500 bg-chassis-900 px-2 py-[3px] text-small text-text-hi focus:border-amber-500 focus:outline-none"
            />,
          )}
          {field("user", input("user"))}
          {field("database", input("database"))}
          {field(
            "password",
            input("password_ref", "vault:prod/orders (optional)"),
            "A vault reference only. Leave empty to type the password at connect time; it is never saved.",
          )}
        </>
      )}
      <div className="mt-4 flex gap-2">
        <button
          type="submit"
          disabled={!p.name.trim()}
          className="rounded border-hairline border-amber-500 bg-amber-900 px-3 py-1 text-small text-amber-500 disabled:opacity-40"
        >
          save
        </button>
        <button type="button" onClick={onCancel} className="px-3 py-1 text-small text-text-mid hover:text-text-hi">
          cancel
        </button>
      </div>
    </form>
  );
}

/* ---- Query editor + result grid ---------------------------------------------- */

function QueryWorkspace({
  opened,
  active,
  onDisconnect,
}: {
  opened: DbOpened;
  /** False while the panel is hidden: the result poll pauses. */
  active: boolean;
  onDisconnect: () => void;
}) {
  const activeRef = useRef(active);
  activeRef.current = active;
  const session = opened.session;
  const [sql, setSql] = useState(
    opened.engine === "sqlite" ? "select name, type from sqlite_master;" : "select 1;",
  );
  const [seq, setSeq] = useState(0);
  const [win, setWin] = useState<DbWindow | null>(null);
  const [sort, setSort] = useState<Sort | null>(null);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const editorRef = useRef<HTMLTextAreaElement | null>(null);

  // Visible range from the table, kept in a ref: the poll reads it, and
  // changing it must not restart the poll.
  const range = useRef({ start: 0, end: 60 });
  /** Something the visible window depends on changed since the last fetch. */
  const stale = useRef(true);
  const runId = useRef(0);
  const seqRef = useRef(seq);
  seqRef.current = seq;

  const run = useCallback(async () => {
    const el = editorRef.current;
    // Run the selection if there is one — the habit every SQL client teaches.
    const text =
      el && el.selectionStart !== el.selectionEnd ? sql.slice(el.selectionStart, el.selectionEnd) : sql;
    setError(null);
    setSort(null);
    setFilter("");
    setWin(null);
    range.current = { start: 0, end: 60 };
    stale.current = true;
    // Two quick runs can resolve out of order; only the latest one counts, or
    // `seq` would settle on the older value and every window would be ignored.
    const mine = ++runId.current;
    try {
      const s = await dbRun(session, text);
      if (mine === runId.current) setSeq(s);
    } catch (e) {
      if (mine === runId.current) setError(String(e));
    }
  }, [session, sql]);

  // Poll the visible window. Cheap: ~60 formatted rows, and nothing at all
  // crosses IPC beyond them.
  useEffect(() => {
    if (seq === 0) return;
    let alive = true;
    let busy = false;
    const tick = () => {
      // A finished result changes only when the view does (scroll, sort,
      // filter), and each of those marks it stale. Otherwise there is nothing
      // to fetch — this used to cost an IPC round trip and a re-render of the
      // whole grid five times a second, forever.
      if (busy || !stale.current || !activeRef.current) return;
      busy = true;
      const { start, end } = range.current;
      dbWindow(session, start, end - start + 1)
        .then((w) => {
          if (!alive || !w || w.seq !== seqRef.current) return;
          if (w.done && !w.sorting) stale.current = false;
          setWin((prev) =>
            prev &&
            prev.seq === w.seq &&
            prev.generation === w.generation &&
            prev.offset === w.offset &&
            prev.rows.length === w.rows.length
              ? prev // identical: keep the old object, skip the re-render
              : w,
          );
        })
        .catch((e: unknown) => alive && setError(String(e)))
        .finally(() => {
          busy = false;
        });
    };
    tick();
    const t = window.setInterval(tick, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [seq, session]);

  // Sort and filter are applied in Rust. Filter is debounced: each keystroke
  // re-scans the whole result.
  useEffect(() => {
    if (seq === 0) return;
    const t = window.setTimeout(() => {
      const col = sort ? Number(sort.column) : null;
      dbSetView(session, col, sort?.dir === "desc", filter)
        .then(() => {
          stale.current = true;
        })
        .catch((e: unknown) => setError(String(e)));
    }, 200);
    return () => window.clearTimeout(t);
  }, [filter, seq, session, sort]);

  const columns: Column<Row>[] = useMemo(
    () =>
      (win?.columns ?? []).map((c, i) => ({
        id: String(i),
        header: c.name,
        width: c.kind === "text" ? 200 : 110,
        align: c.kind === "text" || c.kind === "bool" ? "left" : "right",
        text: (r: Row) => r.cells[i] ?? "NULL",
        render: (r: Row) =>
          r.cells[i] === null ? <span className="text-text-lo">NULL</span> : r.cells[i],
      })),
    [win?.columns],
  );

  const rowAt = useCallback(
    (i: number): Row | undefined => {
      if (!win) return undefined;
      const j = i - win.offset;
      const cells = win.rows[j];
      return cells ? { index: i, cells } : undefined;
    },
    [win],
  );

  const onVisibleRange = useCallback((start: number, end: number) => {
    const r = range.current;
    if (r.start !== start || r.end !== end) stale.current = true;
    range.current = { start, end };
  }, []);

  const status = (() => {
    if (!win) return seq === 0 ? "" : "running…";
    if (win.error) return "failed";
    const t = win.elapsed_ms !== null ? ` · ${win.elapsed_ms} ms` : "";
    if (win.columns.length === 0) {
      return win.done ? `${win.message ?? `${win.affected ?? 0} row(s) affected`}${t}` : "running…";
    }
    if (win.sorting) return `loading all rows to sort/filter · ${win.loaded.toLocaleString()} so far`;
    const shown = sort || filter ? `${win.view_rows.toLocaleString()} of ` : "";
    const more = win.done ? "" : " · more on scroll";
    return `${shown}${win.loaded.toLocaleString()} rows${win.truncated ? " (capped)" : ""}${more}${t}`;
  })();

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ModuleHeader title="SQL" detail={opened.label} status={{ tone: "live", text: "connected" }}>
        <ToolButton onClick={() => void run()} title="Run (Ctrl+Enter); runs the selection if any">
          ▶ run
        </ToolButton>
        <ToolButton
          disabled={!win || win.done}
          onClick={() => void dbCancel(session).catch((e: unknown) => setError(String(e)))}
        >
          cancel
        </ToolButton>
        <ToolButton onClick={onDisconnect}>disconnect</ToolButton>
      </ModuleHeader>

      <textarea
        ref={editorRef}
        value={sql}
        onChange={(e) => setSql(e.target.value)}
        onKeyDown={(e) => {
          // Ctrl+Enter on every platform: ⌘⏎ is the zoom chord on macOS.
          if (e.key === "Enter" && e.ctrlKey && !e.shiftKey) {
            e.preventDefault();
            e.stopPropagation();
            void run();
          }
        }}
        spellCheck={false}
        aria-label="SQL"
        className="h-[150px] shrink-0 resize-y border-b-hairline border-bezel-500 bg-chassis-900 p-3
                   font-[family-name:var(--font-term)] text-small text-text-hi outline-none"
      />

      <div className="flex h-[30px] shrink-0 items-center gap-3 border-b-hairline border-bezel-500 px-3">
        <span className={`num text-micro ${win?.error ? "text-red-500" : "text-text-mid"}`}>{status}</span>
        <span className="ml-auto" />
        {win && win.columns.length > 0 && (
          <FilterInput value={filter} onChange={setFilter} placeholder="filter rows" />
        )}
      </div>

      <ErrorStrip error={error ?? win?.error ?? null} onDismiss={() => setError(null)} />

      {win && win.columns.length > 0 ? (
        <DataTable
          label="Result"
          columns={columns}
          rowCount={win.sorting ? 0 : win.view_rows}
          rowAt={rowAt}
          rowKey={(r) => String(r.index)}
          onVisibleRange={onVisibleRange}
          sort={sort}
          onSort={setSort}
          empty={win.sorting ? "sorting…" : win.done ? "no rows" : "running…"}
        />
      ) : (
        <div className="flex-1" style={{ minHeight: ROW_H }} />
      )}
    </div>
  );
}
