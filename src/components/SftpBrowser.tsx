import { useCallback, useEffect, useState } from "react";
import { SftpPane } from "./SftpPane";
import { TransferList } from "./TransferList";
import {
  baseName,
  localHome,
  remoteJoin,
  sftpHome,
  transferStart,
  type ConnectionInfo,
  type FsEntry,
} from "@/lib/remote";

/**
 * The SFTP dual pane — UI_BUILD_GUIDE Step 10.
 *
 * "Layout: local pane | transfer controls | remote pane."
 *
 * The identity is in the remote pane's header at all times, because §Step 10 is
 * emphatic that multi-identity is the flagship differentiator and "the active
 * identity name is always visible in the pane header".
 */
export function SftpBrowser({ connection }: { connection: ConnectionInfo }) {
  const [localPath, setLocalPath] = useState("");
  const [remotePath, setRemotePath] = useState("");
  const [localSel, setLocalSel] = useState<{ name: string; entry: FsEntry } | null>(null);
  const [remoteSel, setRemoteSel] = useState<{ name: string; entry: FsEntry } | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    void localHome().then(setLocalPath);
  }, []);

  useEffect(() => {
    setRemotePath("");
    sftpHome(connection.key)
      .then(setRemotePath)
      .catch((e: unknown) => setStatus(String(e)));
  }, [connection.key]);

  const bump = useCallback(() => setRefresh((n) => n + 1), []);

  // Queued, not awaited: the engine runs in the background and the list below
  // shows it. Files and directories alike; the engine picks the method.
  const download = async () => {
    if (!remoteSel || !localPath) return;
    setStatus(null);
    try {
      await transferStart(connection.key, "download", remoteJoin(remotePath, remoteSel.name), localPath);
    } catch (e) {
      setStatus(String(e));
    }
  };

  const upload = async () => {
    if (!localSel || !remotePath) return;
    setStatus(null);
    try {
      await transferStart(connection.key, "upload", joinLocal(localPath, localSel.name), remotePath);
    } catch (e) {
      setStatus(String(e));
    }
  };

  const canDownload = !!remoteSel && remoteSel.name !== "..";
  const canUpload = !!localSel && localSel.name !== "..";

  return (
    <section
      data-testid="sftp-browser"
      className="flex min-h-0 flex-1 flex-col overflow-hidden bg-chassis-900"
    >
      <div className="flex min-h-0 flex-1 gap-1 p-1">
        <SftpPane
          title="local"
          side="local"
          path={localPath}
          onPath={setLocalPath}
          selected={localSel?.name ?? null}
          onSelect={(name, entry) => setLocalSel(name && entry ? { name, entry } : null)}
          refreshToken={refresh}
        />

        <div className="flex w-[92px] shrink-0 flex-col items-center justify-center gap-2">
          <button
            type="button"
            onClick={() => void upload()}
            disabled={!canUpload}
            title={canUpload ? `Upload ${localSel?.name ?? ""}` : "Select a local file or folder"}
            className="num w-full rounded border-hairline border-bezel-500 px-1 py-1 text-micro
                       text-text-mid disabled:opacity-30
                       enabled:hover:border-amber-500 enabled:hover:bg-amber-900
                       enabled:hover:text-amber-500"
          >
            → upload
          </button>
          <button
            type="button"
            onClick={() => void download()}
            disabled={!canDownload}
            title={canDownload ? `Download ${remoteSel?.name ?? ""}` : "Select a remote file or folder"}
            className="num w-full rounded border-hairline border-bezel-500 px-1 py-1 text-micro
                       text-text-mid disabled:opacity-30
                       enabled:hover:border-amber-500 enabled:hover:bg-amber-900
                       enabled:hover:text-amber-500"
          >
            ← download
          </button>
        </div>

        <SftpPane
          title={`remote · ${connection.identity}`}
          side="remote"
          connectionKey={connection.key}
          path={remotePath}
          onPath={setRemotePath}
          selected={remoteSel?.name ?? null}
          onSelect={(name, entry) => setRemoteSel(name && entry ? { name, entry } : null)}
          refreshToken={refresh}
        />
      </div>

      <TransferList connectionKey={connection.key} onFinished={bump} />

      <footer className="flex h-status shrink-0 items-center gap-3 border-t-hairline border-bezel-500 bg-chassis-800 px-3">
        <span className="font-condensed uppercase text-eyebrow text-text-lo">sftp</span>
        <span className="num text-micro text-text-mid">
          {connection.user}@{connection.host}:{connection.port}
        </span>
        <span className="num text-micro text-text-lo">identity {connection.identity}</span>
        {status && <span className="num ml-auto text-micro text-red-500">{status}</span>}
      </footer>
    </section>
  );
}

function joinLocal(dir: string, name: string): string {
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + name;
}

export { baseName };
