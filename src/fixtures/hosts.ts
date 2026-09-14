import type { Connection, Environment, Host, HostStatus } from "../types.ts";

/**
 * Host fixtures.
 *
 * Deterministic by construction — a seeded PRNG, never `Math.random()`. Two
 * reasons: `visual-check.spec.ts` stubs `Math.random` to a constant to stop
 * screenshots diffing against themselves, and a benchmark whose input changes
 * per run cannot be compared to yesterday's number (CLAUDE.md §16, "measure
 * before optimizing").
 */

/** mulberry32 — small, fast, good enough for fixture shape. */
function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const ENVIRONMENTS: Environment[] = ["prod", "stage", "dev", "test", "customer"];
const STATUSES: HostStatus[] = ["connected", "idle", "failed", "connecting"];
const ROLES = [
  "linux", "web", "api", "pg", "mysql", "redis", "kafka", "nginx", "worker",
  "bastion", "jump", "build", "cache", "search", "vault", "grafana", "k8s-node",
  "mongo", "rabbit", "minio",
];
const OSES = [
  "ubuntu 22.04", "ubuntu 24.04", "debian 12", "rhel 9", "alpine 3.20",
  "amazonlinux 2023", "windows 2022",
];
const REGIONS = ["eu-west", "eu-north", "us-east", "us-west", "ap-south"];

/** The hosts drawn in the UI_BUILD_GUIDE §0.5 layout sketch, plus the
 *  multi-identity case §0.6 exists to demonstrate. */
export const DEMO_HOSTS: Host[] = [
  {
    id: "h-linux-01",
    name: "linux-01",
    address: "10.20.4.11",
    os: "ubuntu 22.04",
    environment: "prod",
    status: "connected",
    tags: ["prod", "eu-west", "app"],
    connections: [
      { kind: "SSH" },
      { kind: "SFTP", identityName: "deploy" },
      { kind: "SFTP", identityName: "backup" },
      { kind: "DB" },
    ],
  },
  {
    id: "h-linux-02",
    name: "linux-02",
    address: "10.20.4.12",
    os: "ubuntu 22.04",
    environment: "prod",
    status: "connected",
    tags: ["prod", "eu-west", "app"],
    connections: [{ kind: "SSH" }, { kind: "SFTP" }],
  },
  {
    id: "h-pg-primary",
    name: "pg-primary",
    address: "10.20.9.4",
    os: "debian 12",
    environment: "prod",
    status: "idle",
    tags: ["prod", "eu-west", "data"],
    connections: [{ kind: "SSH" }, { kind: "DB" }],
  },
  {
    id: "h-k8s-stg",
    name: "k8s-stg-control",
    address: "10.44.1.2",
    os: "ubuntu 24.04",
    environment: "stage",
    status: "connecting",
    tags: ["stage", "eu-north", "platform"],
    connections: [{ kind: "SSH" }, { kind: "K8S" }, { kind: "DOCKER" }],
  },
  {
    id: "h-releasebox",
    name: "releasebox",
    address: "10.44.7.31",
    os: "rhel 9",
    environment: "dev",
    status: "failed",
    tags: ["dev", "eu-north", "ci"],
    connections: [
      { kind: "SSH" },
      { kind: "SFTP", identityName: "deploy" },
      { kind: "SFTP", identityName: "backup" },
      { kind: "SFTP", identityName: "readonly", readOnly: true },
      { kind: "SFTP", identityName: "audit", readOnly: true },
    ],
  },
  {
    id: "h-decommissioned",
    name: "old-fileserver",
    address: "10.99.0.7",
    os: "windows 2022",
    environment: "test",
    status: "idle",
    tags: ["test", "retired"],
    connections: [], // §Step 4 asks for the no-connections case explicitly
  },
  {
    id: "h-customer-a",
    name: "customer-a-edge",
    address: "203.0.113.44",
    os: "alpine 3.20",
    environment: "customer",
    status: "connected",
    tags: ["customer", "ap-south", "edge"],
    connections: [{ kind: "SSH" }, { kind: "HTTP" }],
  },
];

/**
 * `n` synthetic hosts for the Step 5 budget test. Names are realistic enough
 * that fuzzy ranking is meaningful — a fixture of "host-0001…host-9999" would
 * make the scorer look far better than it is, because every candidate would
 * share a prefix and nothing would compete.
 */
/** Fixed seed: the same `n` must always produce the same hosts. */
export const FIXTURE_SEED = 0x09577_10;

export function generateHosts(n: number, seed = FIXTURE_SEED): Host[] {
  const rand = prng(seed);
  const pick = <T,>(xs: readonly T[]): T => xs[Math.floor(rand() * xs.length)] as T;

  const hosts: Host[] = new Array(n);
  for (let i = 0; i < n; i++) {
    const env = pick(ENVIRONMENTS);
    const region = pick(REGIONS);
    const role = pick(ROLES);
    const ordinal = String((i % 97) + 1).padStart(2, "0");

    const connections: Connection[] = [{ kind: "SSH" }];
    const extra = rand();
    if (extra > 0.35) connections.push({ kind: "SFTP", identityName: "deploy" });
    if (extra > 0.8) connections.push({ kind: "SFTP", identityName: "backup" });
    if (role.startsWith("pg") || role === "mysql" || role === "mongo") connections.push({ kind: "DB" });
    if (role.startsWith("k8s")) connections.push({ kind: "K8S" });
    if (rand() > 0.9) connections.push({ kind: "DOCKER" });

    hosts[i] = {
      id: `h-${i}`,
      name: `${role}-${region.split("-")[1]}-${ordinal}`,
      address: `10.${20 + (i % 200)}.${i % 251}.${(i * 7) % 254}`,
      os: pick(OSES),
      environment: env,
      status: pick(STATUSES),
      tags: [env, region, role],
      connections,
    };
  }
  return hosts;
}
