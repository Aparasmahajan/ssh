import type { StoredHost } from "@/lib/workspace";
import type { Environment } from "@/types";

/**
 * The landing screen — saved hosts, ready to open.
 *
 * The Termius idea: when nothing is running, the app should show you what you
 * *can* run rather than an empty rectangle. Every saved host is one click from a
 * shell, and each identity is one click from its own SFTP pane, which is §0.6's
 * "each chip is a click target that opens that connection type directly... the
 * whole product thesis made clickable".
 */

const ENV_VAR: Record<Environment, string> = {
  prod: "var(--env-prod)",
  stage: "var(--env-stage)",
  dev: "var(--env-dev)",
  test: "var(--env-test)",
  customer: "var(--env-customer)",
};

export function Landing({
  hosts,
  onOpenShell,
  onOpenSftp,
  onLocalShell,
  onNewHost,
  onEditHost,
}: {
  hosts: readonly StoredHost[];
  onOpenShell: (h: StoredHost, identityName?: string) => void;
  onOpenSftp: (h: StoredHost, identityName?: string) => void;
  onLocalShell: () => void;
  onNewHost: () => void;
  onEditHost: (h: StoredHost) => void;
}) {
  return (
    <div className="min-h-0 flex-1 overflow-auto bg-chassis-900 p-6">
      <div className="mx-auto max-w-[980px]">
        <header className="mb-5 flex items-baseline gap-3">
          <h1 className="font-condensed uppercase text-large tracking-[0.08em] text-text-hi">
            OpsPilot
          </h1>
          <span className="text-small text-text-lo">
            {hosts.length === 0
              ? "No saved hosts yet."
              : `${hosts.length} saved host${hosts.length === 1 ? "" : "s"}`}
          </span>

          <span className="ml-auto flex gap-2">
            <button
              type="button"
              onClick={onLocalShell}
              className="border-hairline border-bezel-500 px-3 py-1 text-small text-text-mid
                         hover:border-amber-500 hover:text-amber-500"
            >
              Local shell
            </button>
            <button
              type="button"
              onClick={onNewHost}
              className="border-hairline border-amber-500 bg-amber-900 px-3 py-1 text-small
                         text-amber-500"
            >
              + Add host
            </button>
          </span>
        </header>

        {hosts.length === 0 ? (
          <div className="border-hairline border-bezel-500 p-6">
            <p className="mb-2 text-small text-text-mid">
              Add a host to get started, or open a local shell.
            </p>
            <p className="text-micro text-text-lo">
              Hosts are saved as TOML under{" "}
              <span className="num">~/.config/opspilot/workspaces/</span> — readable, diffable, and
              safe to keep in git. No secret is ever written there: identities hold a key path, and
              passwords are asked for at connect time.
            </p>
          </div>
        ) : (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(300px,1fr))] gap-3">
            {hosts.map((h) => (
              <article
                key={h.id}
                className="group relative flex flex-col gap-2 overflow-hidden border-hairline
                           border-bezel-500 bg-chassis-800 p-3 pl-4"
              >
                {/* §0.2: environment colour appears in exactly one place — a 3px
                    left edge bar. Never as a background tint. */}
                <span
                  data-testid="env-tie"
                  aria-hidden
                  className="absolute inset-y-0 left-0 w-[3px]"
                  style={{ background: ENV_VAR[h.environment] }}
                />

                <div className="flex items-baseline gap-2">
                  <span className="truncate text-small font-medium text-text-hi">{h.name}</span>
                  <span className="font-condensed uppercase text-eyebrow text-text-lo">
                    {h.environment}
                  </span>
                  <button
                    type="button"
                    onClick={() => onEditHost(h)}
                    aria-label={`Edit ${h.name}`}
                    className="ml-auto text-micro text-text-lo opacity-0 transition-opacity
                               duration-hover group-hover:opacity-100 hover:text-amber-500"
                  >
                    ⋮
                  </button>
                </div>

                <p className="num truncate text-micro text-text-lo">
                  {h.address}:{h.port}
                  {h.os ? ` · ${h.os}` : ""}
                </p>

                <div className="flex flex-wrap gap-1">
                  {h.identities.length === 0 ? (
                    <span className="text-micro text-text-lo">no identities — edit to add one</span>
                  ) : (
                    h.identities.map((idn) => (
                      <span key={idn.id} className="flex gap-1">
                        <Chip
                          label={`SSH·${idn.name}`}
                          title={`Open a shell as ${idn.user}`}
                          onClick={() => onOpenShell(h, idn.name)}
                        />
                        <Chip
                          label={`SFTP·${idn.name}`}
                          title={
                            idn.read_only
                              ? `Browse files as ${idn.user} (read only)`
                              : `Browse files as ${idn.user}`
                          }
                          onClick={() => onOpenSftp(h, idn.name)}
                        />
                      </span>
                    ))
                  )}
                </div>

                {h.forwards.length > 0 && (
                  <p className="num text-micro text-text-lo">
                    {h.forwards.length} port forward{h.forwards.length === 1 ? "" : "s"} saved
                  </p>
                )}
              </article>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function Chip({
  label,
  title,
  onClick,
}: {
  label: string;
  title: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      className="num rounded border-hairline border-bezel-500 bg-transparent px-[5px] uppercase
                 text-micro leading-[16px] text-text-mid transition-colors duration-hover
                 hover:border-amber-500 hover:bg-amber-900 hover:text-amber-500"
      style={{ letterSpacing: "var(--tracking-chip)" }}
    >
      {label}
    </button>
  );
}
