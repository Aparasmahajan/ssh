import { invoke } from "@tauri-apps/api/core";
import type { Environment, Host as UiHost } from "@/types";

/**
 * The persisted workspace — hosts, identities and forwards from
 * `~/.config/opspilot/workspaces/*.toml`.
 *
 * These are the *stored* shapes, matching `core-store::model` field for field.
 * They are converted to the UI's `Host` type by `toUiHost`, because the tree
 * cares about connection chips and status while the store cares about auth and
 * forwards, and merging the two would make both worse.
 */

export interface StoredIdentity {
  id: string;
  name: string;
  user: string;
  key_path?: string | null;
  password_ref?: string | null;
  /** Vault reference for an encrypted `key_path`'s passphrase. */
  passphrase_ref?: string | null;
  /** Offer the SSH agent's keys. Absent means true: the file only says `false`. */
  use_agent?: boolean;
  read_only?: boolean;
}

export type StoredForwardKind = "local" | "remote" | "dynamic";

export interface StoredForward {
  id: string;
  name: string;
  kind: StoredForwardKind;
  bind_address: string;
  bind_port: number;
  target_host?: string | null;
  target_port?: number | null;
  auto_start?: boolean;
}

export interface StoredHost {
  id: string;
  name: string;
  address: string;
  port: number;
  os: string;
  environment: Environment;
  tags: string[];
  identities: StoredIdentity[];
  forwards: StoredForward[];
  jump_host?: string | null;
}

export interface Workspace {
  schema_version: number;
  name: string;
  host: StoredHost[];
}

export interface KeyInfo {
  path: string;
  name: string;
  algorithm: string;
  fingerprint: string | null;
  comment: string;
  encrypted: boolean;
  has_public: boolean;
}

export const workspaceList = () => invoke<string[]>("workspace_list");
export const workspaceLoad = (name: string) => invoke<Workspace>("workspace_load", { name });
export const hostUpsert = (workspace: string, host: StoredHost) =>
  invoke<Workspace>("host_upsert", { workspace, host });
export const hostDelete = (workspace: string, hostId: string) =>
  invoke<Workspace>("host_delete", { workspace, hostId });
export const keysList = () => invoke<KeyInfo[]>("keys_list");

/**
 * A counter that moves whenever a config file changes on disk (§12.2).
 *
 * Polled, not pushed: one save in Vim is several filesystem events, and a
 * counter collapses them into a single reload for free.
 */
export const configGeneration = () => invoke<number>("config_generation");

/* ---- Forwards --------------------------------------------------------------- */

export interface ForwardStatus {
  id: string;
  connection_key: string;
  kind: "local" | "dynamic" | "remote";
  bound: string;
  target: string | null;
  connections: number;
  active: number;
  bytes_in: number;
  bytes_out: number;
  errors: number;
}

export const forwardStart = (args: {
  connectionKey: string;
  id: string;
  kind: StoredForwardKind;
  bindAddress: string;
  bindPort: number;
  targetHost?: string;
  targetPort?: number;
}) =>
  invoke<ForwardStatus>("forward_start", {
    connectionKey: args.connectionKey,
    id: args.id,
    kind: args.kind,
    bindAddress: args.bindAddress,
    bindPort: args.bindPort,
    targetHost: args.targetHost ?? null,
    targetPort: args.targetPort ?? null,
  });

export const forwardStop = (id: string) => invoke<void>("forward_stop", { id });
export const forwardList = () => invoke<ForwardStatus[]>("forward_list");

/** One saved forward's outcome when its host connected. */
export interface AutoStarted {
  id: string;
  name: string;
  started: boolean;
  /** Set when `started` is false — including the benign "already running". */
  error: string | null;
}

/**
 * Start the saved forwards flagged `auto_start` on a host that just connected.
 *
 * Failures come back *inside* the list rather than as a rejection: a tunnel that
 * cannot bind must not cost the user the session they actually asked for.
 */
export const forwardAutoStart = (connectionKey: string, workspace: string, hostId: string) =>
  invoke<AutoStarted[]>("forward_auto_start", { connectionKey, workspace, hostId });

/* ---- Conversion -------------------------------------------------------------- */

/**
 * Stored host → the shape the tree renders.
 *
 * Every identity becomes an `SFTP·name` chip, which is §0.6's rule that the
 * multi-identity feature "is invisible unless the UI shows the identity name, so
 * it always does".
 */
export function toUiHost(h: StoredHost): UiHost {
  const connections: UiHost["connections"] = [];
  const seen = new Set<string>();

  for (const idn of h.identities) {
    if (!seen.has("SSH")) {
      connections.push({ kind: "SSH" });
      seen.add("SSH");
    }
    connections.push(
      idn.read_only
        ? { kind: "SFTP", identityName: idn.name, readOnly: true }
        : { kind: "SFTP", identityName: idn.name },
    );
  }
  if (h.forwards.length > 0) connections.push({ kind: "HTTP" });

  return {
    id: h.id,
    name: h.name,
    address: h.address,
    os: h.os,
    environment: h.environment,
    // Status is a live property; nothing is connected just because it is saved.
    status: "idle",
    tags: h.tags,
    connections,
  };
}

export function emptyHost(): StoredHost {
  return {
    id: `h-${Date.now().toString(36)}`,
    name: "",
    address: "",
    port: 22,
    os: "",
    environment: "dev",
    tags: [],
    identities: [
      {
        id: `i-${Date.now().toString(36)}`,
        name: "default",
        user: "",
        key_path: null,
        password_ref: null,
        passphrase_ref: null,
        read_only: false,
      },
    ],
    forwards: [],
    jump_host: null,
  };
}
