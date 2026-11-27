import { invoke } from "@tauri-apps/api/core";

export type KubeKind = "pods" | "deployments" | "services" | "nodes" | "namespaces";

export interface KubeContext {
  name: string;
  cluster: string;
  user: string;
  namespace: string | null;
}

export interface KubeContexts {
  current: string | null;
  contexts: KubeContext[];
  source: string;
}

/** Mirrors `opspilot_core_kube::Row`. */
export interface KubeRow {
  uid: string;
  name: string;
  namespace: string;
  cells: string[];
  created: number;
  tone: "live" | "warn" | "bad" | "idle";
  containers: string[];
  replicas: number | null;
}

export interface KubeSnapshot {
  generation: number;
  kind: KubeKind;
  columns: string[];
  synced: boolean;
  error: string | null;
  /** "watchlist" or "list+watch" — how the informer bootstrapped. */
  mode: string;
  rows: KubeRow[];
}

export const kubeContexts = () => invoke<KubeContexts>("k8s_contexts");

export const kubeSnapshot = (context: string, kind: KubeKind, namespace: string, since: number) =>
  invoke<KubeSnapshot | null>("k8s_snapshot", { context, kind, namespace, since });

export const kubeAction = (
  context: string,
  action: "delete-pod" | "scale" | "rollout-restart",
  namespace: string,
  name: string,
  replicas?: number,
) =>
  invoke<void>(
    "k8s_action",
    replicas === undefined ? { context, action, namespace, name } : { context, action, namespace, name, replicas },
  );

export const kubeYaml = (context: string, kind: KubeKind, namespace: string, name: string) =>
  invoke<string>("k8s_yaml", { context, kind, namespace, name });

/** "3d", "4h", "12m", "40s" — kubectl's AGE column. */
export function age(createdSecs: number, nowSecs: number): string {
  if (!createdSecs) return "";
  const s = Math.max(0, nowSecs - createdSecs);
  if (s < 120) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 120) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}
