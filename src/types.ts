/** Domain types shared by the tree, the palette, and (later) the Rust bridge. */

export type Environment = "prod" | "stage" | "dev" | "test" | "customer";

/** UI_BUILD_GUIDE §0.6 status dot states. */
export type HostStatus = "connected" | "idle" | "failed" | "connecting";

export type ConnectionKind = "SSH" | "SFTP" | "DB" | "K8S" | "DOCKER" | "HTTP";

/**
 * `identityName` is the whole point of the product thesis: CLAUDE.md §8.1 keeps
 * *separate transports* per identity, and §0.6 insists the name is always shown
 * ("SFTP·deploy") because the multi-identity feature is invisible otherwise.
 */
export interface Connection {
  kind: ConnectionKind;
  identityName?: string;
  /** Read-only identities disable rather than hide mutating controls (§Step 10). */
  readOnly?: boolean;
}

export interface Host {
  id: string;
  name: string;
  /** e.g. "10.20.4.11" */
  address: string;
  /** e.g. "ubuntu 22.04" */
  os: string;
  environment: Environment;
  status: HostStatus;
  tags: string[];
  connections: Connection[];
}
