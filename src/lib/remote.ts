import { invoke } from "@tauri-apps/api/core";

/**
 * SSH and SFTP IPC.
 *
 * Credentials go one way only. Nothing here ever reads a password back out of
 * the core — `ConnectionInfo` deliberately carries no secret, so a compromised
 * webview cannot ask for one (the reasoning behind CLAUDE.md §12.3's separate
 * secrets broker, applied as far as it can be before that broker exists).
 */

export interface AuthSpec {
  /** Typed for this one connect. Prefer `passwordRef` for anything saved. */
  password?: string;
  keyPath?: string;
  passphrase?: string;
  /**
   * A vault reference — `vault:prod/deploy`. Resolved natively, so the
   * plaintext never crosses IPC in either direction (CLAUDE.md §12.3).
   *
   * Takes precedence over `password` when both are present: a host configured
   * to use the vault must not be authenticated with a stale typed value.
   */
  passwordRef?: string;
  passphraseRef?: string;
  /** Offer the SSH agent's keys. Defaults to true, as with `ssh`. */
  useAgent?: boolean;
}

/** What the running SSH agent holds. Public keys only. */
export interface AgentInfo {
  available: boolean;
  /** `ED25519 SHA256:… comment`, one per usable key. */
  keys: string[];
  error: string | null;
}

export type HostKeyPolicy = "accept-new" | "strict" | "verify-only";

export interface ConnectRequest {
  host: string;
  port: number;
  user: string;
  /** Names the identity ("deploy", "backup"). Part of the transport pool key. */
  identity: string;
  auth: AuthSpec;
  hostKeyPolicy?: HostKeyPolicy;
}

export interface ConnectionInfo {
  key: string;
  host: string;
  port: number;
  user: string;
  identity: string;
}

export interface FsEntry {
  name: string;
  kind: "file" | "dir" | "symlink" | "other";
  size: number;
  modified: number | null;
  /** "rwxr-xr-x", or "?????????" where the platform has no mode bits. */
  permissions: string;
  /** "0755" */
  octal: string | null;
}

export interface Listing {
  path: string;
  entries: FsEntry[];
}

export interface LocalListing {
  path: string;
  parent: string | null;
  entries: FsEntry[];
}

/* ---- SSH ------------------------------------------------------------------ */

export const sshConnect = (request: ConnectRequest) =>
  invoke<ConnectionInfo>("ssh_connect", {
    request: {
      host: request.host,
      port: request.port,
      user: request.user,
      identity: request.identity,
      auth: {
        password: request.auth.password ?? null,
        key_path: request.auth.keyPath ?? null,
        passphrase: request.auth.passphrase ?? null,
        password_ref: request.auth.passwordRef ?? null,
        passphrase_ref: request.auth.passphraseRef ?? null,
        use_agent: request.auth.useAgent ?? null,
      },
      host_key_policy: request.hostKeyPolicy ?? null,
    },
  });

export const sshAgentStatus = () => invoke<AgentInfo>("ssh_agent_status");

export const sshDisconnect = (key: string) => invoke<void>("ssh_disconnect", { key });
export const sshConnections = () => invoke<ConnectionInfo[]>("ssh_connections");

/** Opens a remote shell as a session the terminal treats like any other. */
export const sshOpenShell = (key: string, rows: number, cols: number) =>
  invoke<number>("ssh_open_shell", { key, rows, cols });

/* ---- SFTP ------------------------------------------------------------------ */

export const sftpHome = (key: string) => invoke<string>("sftp_home", { key });
export const sftpList = (key: string, path: string) =>
  invoke<{ path: string; entries: FsEntry[] }>("sftp_list", { key, path });
export const sftpMkdir = (key: string, path: string) => invoke<void>("sftp_mkdir", { key, path });
export const sftpRename = (key: string, from: string, to: string) =>
  invoke<void>("sftp_rename", { key, from, to });
export const sftpRemove = (key: string, path: string, isDir: boolean) =>
  invoke<void>("sftp_remove", { key, path, isDir });

export const sftpDownload = (key: string, remote: string, local: string) =>
  invoke<number>("sftp_download", { key, remote, local });
export const sftpUpload = (key: string, local: string, remote: string) =>
  invoke<number>("sftp_upload", { key, local, remote });

/* ---- Local ----------------------------------------------------------------- */

export const localHome = () => invoke<string>("local_home");
export const localList = (path: string) => invoke<LocalListing>("local_list", { path });
export const localMkdir = (path: string) => invoke<void>("local_mkdir", { path });
export const localRemove = (path: string, isDir: boolean) =>
  invoke<void>("local_remove", { path, isDir });
export const localRename = (from: string, to: string) =>
  invoke<void>("local_rename", { from, to });

/* ---- Helpers ---------------------------------------------------------------- */

/** Remote paths are always POSIX, whatever the client platform is. */
export function remoteJoin(dir: string, name: string): string {
  if (name === "..") {
    const up = dir.replace(/\/+$/, "").split("/").slice(0, -1).join("/");
    return up === "" ? "/" : up;
  }
  return dir.replace(/\/+$/, "") + "/" + name;
}

export function baseName(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

/** Byte counts in a fixed set of units so the column does not jump about. */
export function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function humanTime(unixSeconds: number | null): string {
  if (unixSeconds === null) return "";
  const d = new Date(unixSeconds * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
