import { invoke } from "@tauri-apps/api/core";

/**
 * Vault IPC — CLAUDE.md §12.3.
 *
 * Note what this module cannot do: **read a secret**. There is no `vaultGet`,
 * because the backend registers no such command. Secrets go in; only record
 * *ids* come out. To authenticate with a stored secret you pass its reference
 * (`vault:prod/deploy`) to `sshConnect`, and the native side resolves it.
 *
 * §12.3 asks for that boundary to be a separate process. It is not — it is the
 * IPC surface — so a compromise of the native side still reaches the vault.
 * But a webview XSS, which is the threat the section names, cannot exfiltrate
 * a password through this file, because there is nothing here to call.
 */

export interface VaultStatus {
  /** Does vault.bin exist? Chooses "create" vs "unlock". */
  exists: boolean;
  unlocked: boolean;
  records: number;
  path: string;
  idleTimeoutSecs: number;
}

export interface AuditRow {
  seq: number;
  action: string;
  id: string;
}

export interface AuditView {
  entries: AuditRow[];
  /** Sequence number where the hash chain breaks, or null if it verifies. */
  brokenAt: number | null;
}

/** serde renames to snake_case on the wire; normalise once, here. */
function normalise(s: Record<string, unknown>): VaultStatus {
  return {
    exists: Boolean(s.exists),
    unlocked: Boolean(s.unlocked),
    records: Number(s.records ?? 0),
    path: String(s.path ?? ""),
    idleTimeoutSecs: Number(s.idle_timeout_secs ?? 0),
  };
}

export const vaultStatus = () =>
  invoke<Record<string, unknown>>("vault_status").then(normalise);

export const vaultCreate = (password: string) =>
  invoke<Record<string, unknown>>("vault_create", { password }).then(normalise);

export const vaultUnlock = (password: string) =>
  invoke<Record<string, unknown>>("vault_unlock", { password }).then(normalise);

export const vaultLock = () =>
  invoke<Record<string, unknown>>("vault_lock").then(normalise);

/** Record ids only. */
export const vaultList = () => invoke<string[]>("vault_list");

export const vaultSet = (id: string, secret: string) =>
  invoke<string[]>("vault_set", { id, secret });

export const vaultRemove = (id: string) => invoke<string[]>("vault_remove", { id });

export const vaultChangePassword = (password: string) =>
  invoke<Record<string, unknown>>("vault_change_password", { password }).then(normalise);

export const vaultAudit = (limit = 200) =>
  invoke<{ entries: AuditRow[]; broken_at: number | null }>("vault_audit", { limit }).then(
    (v): AuditView => ({ entries: v.entries, brokenAt: v.broken_at }),
  );

/* ---- References ------------------------------------------------------------ */

export const REF_PREFIX = "vault:";

export const isVaultRef = (s: string | undefined | null): boolean =>
  typeof s === "string" && s.startsWith(REF_PREFIX) && s.length > REF_PREFIX.length;

export const makeRef = (id: string) => `${REF_PREFIX}${id}`;

export const refId = (ref: string) =>
  ref.startsWith(REF_PREFIX) ? ref.slice(REF_PREFIX.length) : ref;

/**
 * Suggest a record id for a host: `user@host` slugged.
 *
 * Ids are a flat namespace and the user will be looking at them in a list, so a
 * predictable default beats making them invent one.
 */
export function suggestId(user: string, host: string): string {
  const clean = (s: string) => s.trim().toLowerCase().replace(/[^a-z0-9._-]+/g, "-");
  return `${clean(host)}/${clean(user) || "user"}`;
}
