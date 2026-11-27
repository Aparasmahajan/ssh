import { useCallback, useEffect, useState } from "react";
import {
  baseName,
  humanSize,
  humanTime,
  localList,
  remoteJoin,
  sftpList,
  type FsEntry,
} from "@/lib/remote";

/**
 * One side of the SFTP dual pane. UI_BUILD_GUIDE Step 10.
 *
 * Both sides use this component and the same `FsEntry` wire type, so local and
 * remote sort identically and line up column for column — two panes that order
 * themselves differently are hard to compare at a glance, which is the whole
 * job of a dual-pane browser.
 *
 * Per Step 10: size and modified in Plex Mono tabular-nums, permissions shown as
 * both `rwxr-xr-x` and the octal, with the octal in `--text-lo`.
 */
export function SftpPane({
  title,
  side,
  connectionKey,
  path,
  onPath,
  selected,
  onSelect,
  readOnly,
  refreshToken,
}: {
  title: string;
  side: "local" | "remote";
  /** Required for the remote side. */
  connectionKey?: string;
  path: string;
  onPath: (p: string) => void;
  selected: string | null;
  onSelect: (name: string | null, entry: FsEntry | null) => void;
  readOnly?: boolean;
  /** Bump to force a re-list after a transfer or a mutation. */
  refreshToken: number;
}) {
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [parent, setParent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    if (!path) return;
    setLoading(true);
    setError(null);
    try {
      if (side === "local") {
        const l = await localList(path);
        setEntries(l.entries);
        setParent(l.parent);
      } else {
        if (!connectionKey) return;
        const l = await sftpList(connectionKey, path);
        setEntries(l.entries);
        // Remote paths are POSIX regardless of client platform.
        setParent(path === "/" ? null : remoteJoin(path, ".."));
      }
    } catch (e) {
      // Show the failure instead of an empty pane. An empty listing and an
      // unreadable directory look identical otherwise, and that is how people
      // delete the wrong thing.
      setError(String(e));
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, [connectionKey, path, side]);

  useEffect(() => {
    void load();
  }, [load, refreshToken]);

  const open = (e: FsEntry) => {
    if (e.kind === "dir") {
      onSelect(null, null);
      onPath(side === "local" ? joinLocal(path, e.name) : remoteJoin(path, e.name));
    } else {
      onSelect(e.name, e);
    }
  };

  return (
    <section className="flex min-w-0 flex-1 flex-col overflow-hidden border-hairline border-bezel-500 bg-chassis-800">
      <header
        className={`flex items-center gap-2 border-b-hairline border-bezel-500 px-2 py-1 ${
          readOnly ? "text-cyan-400" : "text-text-mid"
        }`}
      >
        <span className="font-condensed uppercase text-eyebrow">{title}</span>
        {/* Step 10: read-only identities render the header in cyan with a lock
            glyph, and mutating controls are disabled rather than hidden. */}
        {readOnly && <span aria-label="read only">🔒</span>}
        <span className="num ml-auto truncate text-micro text-text-lo" title={path}>
          {path}
        </span>
      </header>

      <div className="flex items-center gap-1 border-b-hairline border-bezel-500 px-2 py-1">
        <button
          type="button"
          disabled={!parent}
          onClick={() => parent && onPath(parent)}
          className="num rounded border-hairline border-bezel-500 px-[5px] text-micro
                     text-text-mid disabled:opacity-30 hover:border-amber-500 hover:text-amber-500"
        >
          ↑ up
        </button>
        <button
          type="button"
          onClick={() => void load()}
          className="num rounded border-hairline border-bezel-500 px-[5px] text-micro
                     text-text-mid hover:border-amber-500 hover:text-amber-500"
        >
          refresh
        </button>
        <span className="num ml-auto text-micro text-text-lo">
          {loading ? "…" : `${entries.length} items`}
        </span>
      </div>

      {error && (
        <div className="border-b-hairline border-bezel-500 px-2 py-1 text-micro text-red-500">
          {error}
        </div>
      )}

      <div role="listbox" aria-label={title} className="min-h-0 flex-1 overflow-auto">
        {entries.map((e) => {
          const isSelected = selected === e.name;
          return (
            <div
              key={e.name}
              role="option"
              aria-selected={isSelected}
              tabIndex={0}
              onClick={() => onSelect(e.name, e)}
              onDoubleClick={() => open(e)}
              onKeyDown={(ev) => {
                if (ev.key === "Enter") open(e);
              }}
              // The name keeps at least 120px. With a bare `1fr` it collapsed to
              // nothing whenever the pane was narrower than the fixed columns
              // (~400px, i.e. with the tree and inspector open): the one column
              // that identifies the row vanished first. Narrow panes now scroll
              // sideways instead.
              className="row-hoverable grid cursor-default grid-cols-[minmax(120px,1fr)_84px_128px_150px]
                         items-center gap-2 px-2 py-[1px] text-small"
            >
              <span className="truncate text-text-hi">
                <span aria-hidden className="text-text-lo">
                  {e.kind === "dir" ? "▸ " : e.kind === "symlink" ? "↗ " : "  "}
                </span>
                {e.name}
              </span>
              <span className="num text-right text-micro text-text-mid">
                {e.kind === "dir" ? "" : humanSize(e.size)}
              </span>
              <span className="num text-micro text-text-lo">{humanTime(e.modified)}</span>
              <span className="num text-micro text-text-mid">
                {e.permissions}{" "}
                {e.octal && <span className="text-text-lo">{e.octal}</span>}
              </span>
            </div>
          );
        })}
        {!loading && entries.length === 0 && !error && (
          <p className="p-3 text-small text-text-lo">empty</p>
        )}
      </div>
    </section>
  );
}

/** Local paths use whatever separator the path already uses. */
function joinLocal(dir: string, name: string): string {
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + name;
}

export { baseName };
