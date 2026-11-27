import { invoke } from "@tauri-apps/api/core";

/** Mirrors `opspilot_core_store::databases::DbProfile`. No password field —
 *  by design there is nowhere to store one. */
export interface DbProfile {
  id: string;
  name: string;
  engine: "postgres" | "mysql" | "sqlite";
  host?: string;
  port?: number;
  user?: string;
  database?: string;
  password_ref?: string | null;
  path?: string | null;
}

export interface DbColumn {
  name: string;
  type_name: string;
  kind: "int" | "float" | "bool" | "text" | "numeric";
}

export interface DbWindow {
  seq: number;
  generation: number;
  columns: DbColumn[];
  offset: number;
  rows: (string | null)[][];
  view_rows: number;
  loaded: number;
  done: boolean;
  truncated: boolean;
  sorting: boolean;
  error: string | null;
  affected: number | null;
  message: string | null;
  elapsed_ms: number | null;
}

export interface DbOpened {
  session: number;
  label: string;
  engine: DbProfile["engine"];
}

export const dbProfiles = () => invoke<DbProfile[]>("db_profiles");
export const dbProfileSave = (profile: DbProfile) => invoke<DbProfile[]>("db_profile_save", { profile });
export const dbProfileDelete = (id: string) => invoke<DbProfile[]>("db_profile_delete", { id });

export const dbOpen = (profile: string, password: string | null) =>
  invoke<DbOpened>("db_open", password === null ? { profile } : { profile, password });
export const dbClose = (session: number) => invoke<void>("db_close", { session });
export const dbRun = (session: number, sql: string) => invoke<number>("db_run", { session, sql });
export const dbCancel = (session: number) => invoke<void>("db_cancel", { session });
export const dbWindow = (session: number, offset: number, limit: number) =>
  invoke<DbWindow | null>("db_window", { session, offset, limit });
export const dbSetView = (session: number, sortColumn: number | null, descending: boolean, filter: string) =>
  invoke<void>("db_set_view", sortColumn === null ? { session, descending, filter } : { session, sortColumn, descending, filter });
