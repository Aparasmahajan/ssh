import { useEffect, useState } from "react";
import {
  keysList,
  type KeyInfo,
  type StoredForward,
  type StoredHost,
  type StoredIdentity,
} from "@/lib/workspace";
import type { Environment } from "@/types";
import { makeRef, refId, vaultList, vaultStatus } from "@/lib/vault";

const ENVIRONMENTS: Environment[] = ["prod", "stage", "dev", "test", "customer"];

/**
 * Add or edit a saved host — identities, keys and port forwards.
 *
 * Everything here is written to `workspaces/*.toml`, which is human-readable and
 * hand-editable (§12.2). Nothing typed in this dialog is a secret: identities
 * carry a **key path** or a vault reference, never a password.
 *
 * The secret picker below lists vault record *ids* — it cannot show or fetch a
 * value, because no such command exists (§12.3). Picking one writes
 * `password_ref = "vault:<id>"` into the TOML; the plaintext stays in
 * `vault.bin` and is decrypted natively at connect time.
 */
export function HostEditor({
  host,
  onSave,
  onCancel,
  onDelete,
}: {
  host: StoredHost;
  onSave: (h: StoredHost) => void;
  onCancel: () => void;
  onDelete?: () => void;
}) {
  const [draft, setDraft] = useState<StoredHost>(host);
  const [keys, setKeys] = useState<KeyInfo[]>([]);
  // Vault record ids, for the secret picker. Empty and locked is the normal
  // first-run state, so this must never block the editor from opening.
  const [vaultIds, setVaultIds] = useState<string[]>([]);
  const [vaultUnlocked, setVaultUnlocked] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const st = await vaultStatus();
        if (!live) return;
        setVaultUnlocked(st.unlocked);
        if (st.unlocked) setVaultIds(await vaultList());
      } catch {
        // No desktop shell, or no vault. Neither is an error here.
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    void keysList().then(setKeys).catch(() => setKeys([]));
  }, []);

  const patch = (p: Partial<StoredHost>) => setDraft((d) => ({ ...d, ...p }));

  const patchIdentity = (i: number, p: Partial<StoredIdentity>) =>
    setDraft((d) => ({
      ...d,
      identities: d.identities.map((idn, n) => (n === i ? { ...idn, ...p } : idn)),
    }));

  const addIdentity = () =>
    setDraft((d) => ({
      ...d,
      identities: [
        ...d.identities,
        {
          id: `i-${Date.now().toString(36)}-${d.identities.length}`,
          name: `identity-${d.identities.length + 1}`,
          user: "",
          key_path: null,
          password_ref: null,
          passphrase_ref: null,
          read_only: false,
        },
      ],
    }));

  const patchForward = (i: number, p: Partial<StoredForward>) =>
    setDraft((d) => ({
      ...d,
      forwards: d.forwards.map((f, n) => (n === i ? { ...f, ...p } : f)),
    }));

  const addForward = () =>
    setDraft((d) => ({
      ...d,
      forwards: [
        ...d.forwards,
        {
          id: `f-${Date.now().toString(36)}-${d.forwards.length}`,
          name: `forward-${d.forwards.length + 1}`,
          kind: "local",
          // Loopback, never 0.0.0.0 — the other default exposes a tunnel into
          // production to the whole local network.
          bind_address: "127.0.0.1",
          bind_port: 0,
          target_host: "",
          target_port: 0,
          auto_start: false,
        },
      ],
    }));

  const valid = draft.name.trim() !== "" && draft.address.trim() !== "";

  return (
    <div className="absolute inset-0 z-50 flex items-center justify-center bg-chassis-900/80 p-6">
      <div className="flex max-h-full w-[620px] flex-col border-hairline border-bezel-500 bg-chassis-800">
        <header className="flex items-center gap-2 border-b-hairline border-bezel-500 px-4 py-2">
          <span className="font-condensed uppercase text-eyebrow text-text-lo">
            {host.name ? "edit host" : "new host"}
          </span>
          <span className="num ml-auto text-micro text-text-lo">saved to workspaces/*.toml</span>
        </header>

        <div className="min-h-0 flex-1 overflow-auto p-4">
          <Grid>
            <L>name</L>
            <T value={draft.name} onChange={(v) => patch({ name: v })} placeholder="linux-01" />
            <L>address</L>
            <T
              value={draft.address}
              onChange={(v) => patch({ address: v })}
              placeholder="10.20.4.11"
            />
            <L>port</L>
            <N value={draft.port} onChange={(v) => patch({ port: v })} />
            <L>os</L>
            <T value={draft.os} onChange={(v) => patch({ os: v })} placeholder="ubuntu 22.04" />
            <L>environment</L>
            <select
              value={draft.environment}
              onChange={(e) => patch({ environment: e.target.value as Environment })}
              className="border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                         text-text-hi focus:border-amber-500 focus:outline-none"
            >
              {ENVIRONMENTS.map((e) => (
                <option key={e} value={e}>
                  {e}
                </option>
              ))}
            </select>
            <L>tags</L>
            <T
              value={draft.tags.join(", ")}
              onChange={(v) =>
                patch({ tags: v.split(",").map((t) => t.trim()).filter(Boolean) })
              }
              placeholder="app, eu-west"
            />
          </Grid>

          {/* ---- Identities ---- */}
          <Section
            title="identities"
            hint="Each becomes an SFTP chip on the host row. Separate identities get separate transports (§8.1)."
            onAdd={addIdentity}
          />
          {draft.identities.map((idn, i) => (
            <div key={idn.id} className="mb-2 border-hairline border-bezel-500 p-2">
              <Grid>
                <L>name</L>
                <T value={idn.name} onChange={(v) => patchIdentity(i, { name: v })} />
                <L>user</L>
                <T value={idn.user} onChange={(v) => patchIdentity(i, { user: v })} />
                <L>key</L>
                <select
                  value={idn.key_path ?? ""}
                  onChange={(e) => patchIdentity(i, { key_path: e.target.value || null })}
                  className="num border-hairline border-bezel-500 bg-chassis-900 px-2 py-1
                             text-micro text-text-hi focus:border-amber-500 focus:outline-none"
                >
                  <option value="">— password at connect time —</option>
                  {keys.map((k) => (
                    <option key={k.path} value={k.path}>
                      {k.name} · {k.algorithm}
                      {k.encrypted ? " · encrypted" : ""}
                      {k.comment ? ` · ${k.comment}` : ""}
                    </option>
                  ))}
                </select>
                <L>password</L>
                <SecretPicker
                  value={idn.password_ref ?? null}
                  onChange={(v) => patchIdentity(i, { password_ref: v })}
                  unlocked={vaultUnlocked}
                  ids={vaultIds}
                  emptyLabel="— ask at connect time —"
                />
                {/* Only for an encrypted key: a passphrase reference against an
                    unencrypted key would never be used, and offering it invites
                    the user to store a secret that does nothing. A key the
                    scanner does not recognise gets the control anyway, because
                    "unknown" is not the same as "unencrypted". */}
                {idn.key_path &&
                  (keys.find((k) => k.path === idn.key_path)?.encrypted ?? true) && (
                    <>
                      <L>passphrase</L>
                      <SecretPicker
                        value={idn.passphrase_ref ?? null}
                        onChange={(v) => patchIdentity(i, { passphrase_ref: v })}
                        unlocked={vaultUnlocked}
                        ids={vaultIds}
                        emptyLabel="— ask at connect time —"
                      />
                    </>
                  )}
                <L>read only</L>
                <label className="flex items-center gap-2 text-small text-text-mid">
                  <input
                    type="checkbox"
                    checked={idn.read_only ?? false}
                    onChange={(e) => patchIdentity(i, { read_only: e.target.checked })}
                  />
                  disables mutating SFTP controls rather than hiding them
                </label>
              </Grid>
              {draft.identities.length > 1 && (
                <button
                  type="button"
                  onClick={() =>
                    setDraft((d) => ({
                      ...d,
                      identities: d.identities.filter((_, n) => n !== i),
                    }))
                  }
                  className="mt-1 text-micro text-text-lo hover:text-red-500"
                >
                  remove identity
                </button>
              )}
            </div>
          ))}

          {/* ---- Forwards ---- */}
          <Section
            title="port forwards"
            hint="Local (-L), remote (-R), and dynamic SOCKS (-D)."
            onAdd={addForward}
          />
          {draft.forwards.map((f, i) => (
            <div key={f.id} className="mb-2 border-hairline border-bezel-500 p-2">
              <Grid>
                <L>name</L>
                <T value={f.name} onChange={(v) => patchForward(i, { name: v })} />
                <L>kind</L>
                <select
                  value={f.kind}
                  onChange={(e) =>
                    patchForward(i, { kind: e.target.value as StoredForward["kind"] })
                  }
                  className="border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                             text-text-hi focus:border-amber-500 focus:outline-none"
                >
                  <option value="local">local (-L)</option>
                  <option value="dynamic">dynamic SOCKS5 (-D)</option>
                  <option value="remote">remote (-R)</option>
                </select>
                <L>bind</L>
                <div className="flex gap-1">
                  <T
                    value={f.bind_address}
                    onChange={(v) => patchForward(i, { bind_address: v })}
                  />
                  <N value={f.bind_port} onChange={(v) => patchForward(i, { bind_port: v })} />
                </div>
                {(f.kind === "local" || f.kind === "remote") && (
                  <>
                    <L>target</L>
                    <div className="flex gap-1">
                      <T
                        value={f.target_host ?? ""}
                        onChange={(v) => patchForward(i, { target_host: v })}
                        placeholder="10.20.9.4"
                      />
                      <N
                        value={f.target_port ?? 0}
                        onChange={(v) => patchForward(i, { target_port: v })}
                      />
                    </div>
                  </>
                )}
                <L>on connect</L>
                <label className="flex items-center gap-2 text-small text-text-mid">
                  <input
                    type="checkbox"
                    checked={f.auto_start ?? false}
                    onChange={(e) => patchForward(i, { auto_start: e.target.checked })}
                  />
                  start this forward as soon as the host connects
                </label>
              </Grid>
              <button
                type="button"
                onClick={() =>
                  setDraft((d) => ({ ...d, forwards: d.forwards.filter((_, n) => n !== i) }))
                }
                className="mt-1 text-micro text-text-lo hover:text-red-500"
              >
                remove forward
              </button>
            </div>
          ))}
        </div>

        <footer className="flex items-center gap-2 border-t-hairline border-bezel-500 px-4 py-2">
          {onDelete && (
            <button
              type="button"
              onClick={onDelete}
              className="text-small text-text-lo hover:text-red-500"
            >
              delete host
            </button>
          )}
          <button
            type="button"
            onClick={onCancel}
            className="ml-auto border-hairline border-bezel-500 px-3 py-1 text-small text-text-mid
                       hover:border-text-lo"
          >
            Cancel
          </button>
          <button
            type="button"
            disabled={!valid}
            onClick={() => onSave(draft)}
            className="border-hairline border-amber-500 bg-amber-900 px-3 py-1 text-small
                       text-amber-500 disabled:opacity-40"
          >
            Save
          </button>
        </footer>
      </div>
    </div>
  );
}

function Grid({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-2 grid grid-cols-[92px_1fr] items-center gap-x-2 gap-y-2">{children}</div>
  );
}

function L({ children }: { children: string }) {
  return <span className="font-condensed uppercase text-eyebrow text-text-lo">{children}</span>;
}

function T({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <input
      value={value}
      spellCheck={false}
      onChange={(e) => onChange(e.target.value)}
      {...(placeholder ? { placeholder } : {})}
      className="num min-w-0 flex-1 border-hairline border-bezel-500 bg-chassis-900 px-2 py-1
                 text-small text-text-hi placeholder:text-text-lo
                 focus:border-amber-500 focus:outline-none"
    />
  );
}

function N({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <input
      type="number"
      value={value}
      min={0}
      max={65535}
      onChange={(e) => onChange(Number(e.target.value))}
      className="num w-24 border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                 text-text-hi focus:border-amber-500 focus:outline-none"
    />
  );
}

/**
 * Attach a vault record to an identity, by **id only**.
 *
 * There is no command that returns a secret (§12.3), so this cannot show a
 * value even if it wanted to — it writes `vault:<id>` into the TOML and the
 * plaintext is decrypted natively at connect time.
 *
 * A locked vault still renders the current reference rather than an empty
 * control: hiding it would read as "no secret attached", which is the opposite
 * of the truth.
 */
function SecretPicker({
  value,
  onChange,
  unlocked,
  ids,
  emptyLabel,
}: {
  value: string | null;
  onChange: (v: string | null) => void;
  unlocked: boolean;
  ids: string[];
  emptyLabel: string;
}) {
  if (!unlocked) {
    return (
      <p className="text-micro text-text-lo">
        {value ? (
          <>
            <span className="num text-amber-500">{refId(value)}</span> — unlock the vault to
            change it
          </>
        ) : (
          "unlock the vault to attach a stored secret"
        )}
      </p>
    );
  }
  return (
    <select
      value={value ?? ""}
      onChange={(e) => onChange(e.target.value || null)}
      className="num border-hairline border-bezel-500 bg-chassis-900 px-2 py-1
                 text-micro text-text-hi focus:border-amber-500 focus:outline-none"
    >
      <option value="">{emptyLabel}</option>
      {ids.map((id) => (
        <option key={id} value={makeRef(id)}>
          {id}
        </option>
      ))}
    </select>
  );
}

function Section({
  title,
  hint,
  onAdd,
}: {
  title: string;
  hint: string;
  onAdd: () => void;
}) {
  return (
    <div className="mb-2 mt-4 flex items-baseline gap-2 border-t-hairline border-bezel-500 pt-3">
      <span className="font-condensed uppercase text-eyebrow text-text-mid">{title}</span>
      <span className="text-micro text-text-lo">{hint}</span>
      <button
        type="button"
        onClick={onAdd}
        className="num ml-auto rounded border-hairline border-bezel-500 px-[5px] text-micro
                   text-text-mid hover:border-amber-500 hover:text-amber-500"
      >
        + add
      </button>
    </div>
  );
}
