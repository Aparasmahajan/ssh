import { invoke } from "@tauri-apps/api/core";

/**
 * A terminal session that is not a shell on a PTY: a shell exec'd into a
 * container or pod, or a followed log. The Rust side renders all of them in the
 * ordinary terminal (CLAUDE.md §10: "Log streaming reuses the terminal ring
 * buffer and coalescing rules verbatim"), so to the UI each is just a way of
 * obtaining a session id at a given size.
 *
 * Plain data, so a tab can hold it and a split pane can open a second one.
 */
export type StreamSource =
  | { kind: "docker-exec"; id: string; name: string }
  | { kind: "docker-logs"; id: string; name: string }
  | { kind: "k8s-exec"; context: string; namespace: string; pod: string; container: string | null }
  | { kind: "k8s-logs"; context: string; namespace: string; pod: string; container: string | null };

export function openStream(src: StreamSource, rows: number, cols: number): Promise<number> {
  switch (src.kind) {
    case "docker-exec":
      return invoke<number>("docker_exec", { id: src.id, rows, cols });
    case "docker-logs":
      return invoke<number>("docker_logs", { id: src.id, rows, cols });
    case "k8s-exec":
      return invoke<number>("k8s_exec", { ...target(src), rows, cols });
    case "k8s-logs":
      return invoke<number>("k8s_logs", { ...target(src), rows, cols });
  }
}

function target(src: { context: string; namespace: string; pod: string; container: string | null }) {
  return { context: src.context, namespace: src.namespace, pod: src.pod, container: src.container };
}

/** Short label for the tab and the pane header. */
export function streamTitle(src: StreamSource): string {
  switch (src.kind) {
    case "docker-exec":
      return `⌁ ${src.name}`;
    case "docker-logs":
      return `logs · ${src.name}`;
    case "k8s-exec":
      return `⌁ ${src.pod}`;
    case "k8s-logs":
      return `logs · ${src.pod}`;
  }
}

/** Logs are read-only; typing into them goes nowhere. */
export const isReadOnly = (src: StreamSource) => src.kind.endsWith("-logs");
