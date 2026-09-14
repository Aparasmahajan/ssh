import type { Environment, Host, HostStatus } from "@/types";
import { HostRow } from "@/components/HostRow";

/**
 * Step 4 asks for "a Storybook story covering every status × environment ×
 * compact combination, plus a host with four SFTP identities and one with no
 * connections."
 *
 * This is that matrix as a fixture route (`/?fixture=host-row-gallery`) instead
 * of a Storybook story. Reason: `visual-check.spec.ts` already establishes
 * `/?fixture=<name>` as the harness's entry mechanism, so this needs no new
 * tooling, no second build, and nothing extra against the 250ms cold-start
 * budget — and the same route is directly screenshot-testable by the Playwright
 * suite that already exists. Recorded as D-8 in docs/DECISIONS.md.
 */

const ENVIRONMENTS: Environment[] = ["prod", "stage", "dev", "test", "customer"];
const STATUSES: HostStatus[] = ["connected", "idle", "failed", "connecting"];

const ROW_WIDTH = 260; // the default tree width, so truncation matches production

function cell(env: Environment, status: HostStatus): Host {
  return {
    id: `${env}-${status}`,
    name: `${env}-web-01`,
    address: "10.20.4.11",
    os: "ubuntu 22.04",
    environment: env,
    status,
    tags: [env],
    connections: [{ kind: "SSH" }, { kind: "SFTP", identityName: "deploy" }, { kind: "DB" }],
  };
}

const FOUR_IDENTITIES: Host = {
  id: "four-identities",
  name: "releasebox",
  address: "10.44.7.31",
  os: "rhel 9",
  environment: "stage",
  status: "connected",
  tags: ["stage"],
  connections: [
    { kind: "SSH" },
    { kind: "SFTP", identityName: "deploy" },
    { kind: "SFTP", identityName: "backup" },
    { kind: "SFTP", identityName: "readonly", readOnly: true },
    { kind: "SFTP", identityName: "audit", readOnly: true },
  ],
};

const NO_CONNECTIONS: Host = {
  id: "no-connections",
  name: "old-fileserver",
  address: "10.99.0.7",
  os: "windows 2022",
  environment: "test",
  status: "idle",
  tags: ["test"],
  connections: [],
};

const LONG_ADDRESS: Host = {
  id: "long-address",
  name: "customer-a-edge-gateway-frankfurt",
  address: "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
  os: "amazonlinux 2023",
  environment: "customer",
  status: "connected",
  tags: ["customer"],
  connections: [{ kind: "SSH" }, { kind: "HTTP" }],
};

export function HostRowGallery() {
  return (
    <div className="h-full overflow-auto bg-chassis-900 p-6">
      <h1 className="mb-4 font-condensed uppercase text-eyebrow text-text-lo">
        host row — every status × environment
      </h1>

      <div className="flex gap-6">
        {([false, true] as const).map((compact) => (
          <section key={String(compact)}>
            <h2 className="mb-2 font-condensed uppercase text-eyebrow text-text-mid">
              {compact ? "compact · 26px" : "default · 64px (figma, per §0.9)"}
            </h2>
            <div className="flex gap-4">
              {STATUSES.map((status) => (
                <div key={status}>
                  <p className="mb-1 num text-eyebrow text-text-lo">{status}</p>
                  <div
                    style={{ width: ROW_WIDTH }}
                    className="divide-y divide-bezel-500 border-hairline border-bezel-500 bg-chassis-800"
                  >
                    {ENVIRONMENTS.map((env) => (
                      <HostRow
                        key={env}
                        host={cell(env, status)}
                        width={ROW_WIDTH}
                        compact={compact}
                      />
                    ))}
                  </div>
                </div>
              ))}
            </div>
          </section>
        ))}
      </div>

      <h2 className="mb-2 mt-6 font-condensed uppercase text-eyebrow text-text-mid">edge cases</h2>
      <div
        style={{ width: ROW_WIDTH }}
        className="divide-y divide-bezel-500 border-hairline border-bezel-500 bg-chassis-800"
      >
        <HostRow host={FOUR_IDENTITIES} width={ROW_WIDTH} />
        <HostRow host={NO_CONNECTIONS} width={ROW_WIDTH} />
        <HostRow host={LONG_ADDRESS} width={ROW_WIDTH} />
        <HostRow host={FOUR_IDENTITIES} width={ROW_WIDTH} selected />
      </div>
      <p className="mt-2 max-w-[520px] text-micro text-text-lo">
        Row 3 exercises middle truncation: the IPv6 address keeps its head and tail. Row 4 is the
        selected state — <code className="num">chassis-700</code>, a deliberate 4.4% step, versus the
        ~2% hover.
      </p>
    </div>
  );
}
