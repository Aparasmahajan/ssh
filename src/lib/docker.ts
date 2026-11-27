import { invoke } from "@tauri-apps/api/core";

/** Mirrors `opspilot_core_docker::Container`. */
export interface Container {
  id: string;
  name: string;
  image: string;
  state: "created" | "running" | "paused" | "restarting" | "exited" | "removing" | "dead" | string;
  status: string;
  ports: string;
  created: number;
  project: string | null;
  cpu_pct: number | null;
  mem_bytes: number | null;
  mem_limit: number | null;
}

export interface DockerView {
  generation: number;
  connected: boolean;
  error: string | null;
  version: string | null;
  endpoint: string | null;
  source: string | null;
  containers: Container[];
}

export type DockerAction = "start" | "stop" | "restart" | "pause" | "unpause" | "remove" | "remove-force";

/** `null` when nothing changed since `since` — sample it, never stream it. */
export const dockerSnapshot = (since: number) => invoke<DockerView | null>("docker_snapshot", { since });

export const dockerAction = (id: string, action: DockerAction) =>
  invoke<void>("docker_action", { id, action });
