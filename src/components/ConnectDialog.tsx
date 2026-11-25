import { useEffect, useState } from "react";
import {
  sshAgentStatus,
  sshConnect,
  type AgentInfo,
  type ConnectionInfo,
  type HostKeyPolicy,
} from "@/lib/remote";
import type { StoredHost } from "@/lib/workspace";
import {
  isVaultRef,
  makeRef,
  refId,
  suggestId,
  vaultList,
  vaultSet,
  vaultStatus,
  vaultUnlock,
  type VaultStatus,
} from "@/lib/vault";

/**
 * Connect to a host.
 *
 * Two things here are security decisions, not layout:
 *
 * 1. **The host-key policy is visible and defaults to accept-new**, matching
 *    OpenSSH's own default. It is a control the user can see rather than a
 *    hidden constant, because "strict" is the right choice for anyone who
 *    manages known_hosts deliberately.
 * 2. **A changed host key is reported in full, in red, and cannot be clicked
 *    past.** There is no "connect anyway" button. CLAUDE.md's host-key rule is
 *    that a rotation and a machine-in-the-middle are indistinguishable from
 *    here, so the app does not offer to guess.
 * 3. **A saved credential is sent as a reference, not a value.** When the host
 *    resolves to a `vault:` id, the password field is replaced by the reference
 *    and the plaintext never enters this component at all (CLAUDE.md 12.3).
 */
export function ConnectDialog({
  onConnected,
  onCancel,
  prefill,
  defaultSftp,
}: {
  onConnected: (c: ConnectionInfo, alsoSftp: boolean) => void;
  onCancel: () => void;
  /** A saved host, so connecting is one click plus whatever secret is needed. */
  prefill?: StoredHost;
  /** Pre-tick "open the file browser" when an SFTP chip was the entry point. */
  defaultSftp?: boolean;
}) {
  const first = prefill?.identities[0];
  const [host, setHost] = useState(prefill?.address ?? "");
  const [port, setPort] = useState(prefill?.port ?? 22);
  const [user, setUser] = useState(first?.user ?? "");
  const [identity, setIdentity] = useState(first?.name ?? "default");
  const [password, setPassword] = useState("");
  const [keyPath, setKeyPath] = useState(first?.key_path ?? "");
  const [openSftp, setOpenSftp] = useState(defaultSftp ?? false);
  const [passphrase, setPassphrase] = useState("");
  const [policy, setPolicy] = useState<HostKeyPolicy>("accept-new");
  // On unless the saved identity says otherwise, as with `ssh`.
  const [useAgent, setUseAgent] = useState(first?.use_agent ?? true);
  const [agent, setAgent] = useState<AgentInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /* ---- Vault ------------------------------------------------------------- */

  // The saved host may already point at a vault record. If it does, that is
  // authoritative: the password field is replaced rather than shown empty,
  // because an empty field beside "using vault:prod/deploy" invites the user to
  // type something that would be ignored.
  const savedRef = isVaultRef(first?.password_ref) ? (first?.password_ref as string) : null;
  // The same rule for the key's passphrase, which is a different secret: it
  // unlocks a local file rather than authenticating to the server, and a host
  // can be configured with both.
  const savedPassphraseRef = isVaultRef(first?.passphrase_ref)
    ? (first?.passphrase_ref as string)
    : null;

  const [vault, setVault] = useState<VaultStatus | null>(null);
  const [vaultIds, setVaultIds] = useState<string[]>([]);
  const [secretRef, setSecretRef] = useState<string | null>(savedRef);
  const [passphraseRef, setPassphraseRef] = useState<string | null>(savedPassphraseRef);
  const [masterPw, setMasterPw] = useState("");
  const [saveToVault, setSaveToVault] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const s = await vaultStatus();
        if (!live) return;
        setVault(s);
        if (s.unlocked) setVaultIds(await vaultList());
      } catch {
        // Having no vault is a normal state, not an error worth showing here.
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  // What the agent holds, so "use SSH agent" says whether it can do anything.
  useEffect(() => {
    let live = true;
    sshAgentStatus()
      .then((a) => live && setAgent(a))
      .catch(() => live && setAgent(null));
    return () => {
      live = false;
    };
  }, []);

  const needsUnlock = Boolean(secretRef ?? passphraseRef) && vault?.unlocked === false;

  async function unlockNow() {
    setError(null);
    try {
      const s = await vaultUnlock(masterPw);
      setMasterPw("");
      setVault(s);
      setVaultIds(await vaultList());
    } catch (e) {
      setError(String(e));
    }
  }

  const hostKeyChanged = error?.includes("HAS CHANGED") ?? false;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      // Save BEFORE connecting. Saving afterwards would lose the credential
      // whenever the connect throws for an unrelated reason — the exact moment
      // the user most wants not to retype it.
      let passwordRef: string | undefined;
      if (saveToVault && password && vault?.unlocked) {
        const id = suggestId(user.trim(), host.trim());
        await vaultSet(id, password);
        passwordRef = makeRef(id);
      } else if (secretRef) {
        passwordRef = secretRef;
      }

      const info = await sshConnect({
        host: host.trim(),
        port,
        user: user.trim(),
        identity: identity.trim() || "default",
        auth: {
          // A reference wins; sending the typed value too would leave it in the
          // IPC payload for no purpose.
          password: passwordRef ? "" : password,
          keyPath: keyPath.trim(),
          passphrase: passphraseRef ? "" : passphrase,
          ...(passwordRef ? { passwordRef } : {}),
          ...(passphraseRef ? { passphraseRef } : {}),
          useAgent,
        },
        hostKeyPolicy: policy,
      });
      // Clear the secrets from component state the moment they are no longer
      // needed. Not a substitute for §12.3's zeroized broker, but it shortens
      // the window in which they sit in the webview's heap.
      setPassword("");
      setPassphrase("");
      onConnected(info, openSftp);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="absolute inset-0 z-50 flex items-center justify-center bg-chassis-900/80">
      <form
        onSubmit={submit}
        className="w-[440px] border-hairline border-bezel-500 bg-chassis-800 p-4"
      >
        <p className="mb-3 font-condensed uppercase text-eyebrow text-text-lo">new connection</p>

        <div className="grid grid-cols-[88px_1fr] items-center gap-x-2 gap-y-2">
          <Label>host</Label>
          <Input value={host} onChange={setHost} placeholder="10.20.4.11" autoFocus required />

          <Label>port</Label>
          <input
            type="number"
            value={port}
            min={1}
            max={65535}
            onChange={(e) => setPort(Number(e.target.value))}
            className="num w-24 border-hairline border-bezel-500 bg-chassis-900 px-2 py-1
                       text-small text-text-hi focus:border-amber-500 focus:outline-none"
          />

          <Label>user</Label>
          <Input value={user} onChange={setUser} placeholder="deploy" required />

          <Label>identity</Label>
          <Input value={identity} onChange={setIdentity} placeholder="deploy" />

          <Label>key file</Label>
          <Input value={keyPath} onChange={setKeyPath} placeholder="~/.ssh/id_ed25519" />

          <Label>agent</Label>
          <label
            className="flex items-center gap-2 text-small text-text-mid"
            title={agent?.keys.join("\n") || agent?.error || undefined}
          >
            <input
              type="checkbox"
              checked={useAgent}
              onChange={(e) => setUseAgent(e.target.checked)}
            />
            use SSH agent
            <span className={`num text-micro ${agentReady(agent) ? "text-text-lo" : "text-amber-500"}`}>
              {agentSummary(agent)}
            </span>
          </label>

          <Label>passphrase</Label>
          {passphraseRef ? (
            <div className="flex items-center gap-2">
              <span className="num truncate text-small text-amber-500">{passphraseRef}</span>
              <button
                type="button"
                onClick={() => setPassphraseRef(null)}
                className="shrink-0 text-micro text-text-lo hover:text-text-hi"
              >
                type instead
              </button>
            </div>
          ) : (
            <Input value={passphrase} onChange={setPassphrase} password />
          )}

          <Label>password</Label>
          {secretRef ? (
            <div className="flex items-center gap-2">
              <span className="num truncate text-small text-amber-500">{secretRef}</span>
              <button
                type="button"
                onClick={() => setSecretRef(null)}
                className="shrink-0 text-micro text-text-lo hover:text-text-hi"
              >
                type instead
              </button>
            </div>
          ) : (
            <Input value={password} onChange={setPassword} password />
          )}

          <Label>host key</Label>
          <select
            value={policy}
            onChange={(e) => setPolicy(e.target.value as HostKeyPolicy)}
            className="border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                       text-text-hi focus:border-amber-500 focus:outline-none"
          >
            <option value="accept-new">accept new (record on first use)</option>
            <option value="strict">strict (must be in known_hosts)</option>
            <option value="verify-only">verify only (never record)</option>
          </select>
        </div>

        {/* Vault affordances, shown only when they can actually do something. */}
        {!secretRef && vault?.unlocked && vaultIds.length > 0 && (
          <div className="mt-2 grid grid-cols-[88px_1fr] items-center gap-x-2">
            <Label>saved</Label>
            <select
              value=""
              onChange={(e) => e.target.value && setSecretRef(e.target.value)}
              className="border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                         text-text-hi focus:border-amber-500 focus:outline-none"
            >
              <option value="">use a stored secret…</option>
              {vaultIds.map((id) => (
                <option key={id} value={makeRef(id)}>
                  {id}
                </option>
              ))}
            </select>
          </div>
        )}

        {!secretRef && vault?.unlocked && password.length > 0 && (
          <label className="mt-2 flex items-center gap-2 text-micro text-text-mid">
            <input
              type="checkbox"
              checked={saveToVault}
              onChange={(e) => setSaveToVault(e.target.checked)}
            />
            save to the vault as{" "}
            <span className="num text-amber-500">{suggestId(user.trim(), host.trim())}</span>
          </label>
        )}

        {needsUnlock && (
          <div className="mt-2 border-hairline border-amber-500 p-2">
            <p className="mb-2 text-micro text-text-mid">
              This host uses{" "}
              <span className="num text-amber-500">{refId(secretRef ?? passphraseRef ?? "")}</span>{" "}
              from the vault. Unlock it to connect.
            </p>
            <div className="flex gap-2">
              <input
                type="password"
                value={masterPw}
                autoComplete="off"
                placeholder="master password"
                onChange={(e) => setMasterPw(e.target.value)}
                onKeyDown={(e) => {
                  // This input lives inside the connect form; Enter must unlock,
                  // not submit a connect that is guaranteed to fail.
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void unlockNow();
                  }
                }}
                className="num flex-1 border-hairline border-bezel-500 bg-chassis-900 px-2 py-1
                           text-small text-text-hi focus:border-amber-500 focus:outline-none"
              />
              <button
                type="button"
                disabled={!masterPw}
                onClick={() => void unlockNow()}
                className="border-hairline border-amber-500 px-3 py-1 text-small text-amber-500
                           disabled:opacity-40"
              >
                Unlock
              </button>
            </div>
          </div>
        )}

        <p className="mt-2 text-micro text-text-lo">
          The identity name separates transports on one host — <span className="num">deploy</span>{" "}
          and <span className="num">backup</span> get their own connection and their own
          credentials.
        </p>

        {error && (
          <div
            className={`mt-3 border-hairline p-2 text-micro ${
              hostKeyChanged ? "border-red-500 text-red-500" : "border-bezel-500 text-text-mid"
            }`}
          >
            {hostKeyChanged && (
              <p className="mb-1 font-condensed uppercase text-eyebrow">host key verification failed</p>
            )}
            {error}
          </div>
        )}

        <label className="mt-3 flex items-center gap-2 text-small text-text-mid">
          <input
            type="checkbox"
            checked={openSftp}
            onChange={(e) => setOpenSftp(e.target.checked)}
          />
          open the file browser instead of a shell
        </label>

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="border-hairline border-bezel-500 px-3 py-1 text-small text-text-mid
                       hover:border-text-lo"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={busy || !host.trim() || !user.trim() || needsUnlock}
            className="border-hairline border-amber-500 bg-amber-900 px-3 py-1 text-small
                       text-amber-500 disabled:opacity-40"
          >
            {busy ? "Connecting…" : "Connect"}
          </button>
        </div>
      </form>
    </div>
  );
}

function agentReady(a: AgentInfo | null): boolean {
  return a !== null && a.available && a.keys.length > 0;
}

/** One short phrase. The full key list is the row's tooltip. */
function agentSummary(a: AgentInfo | null): string {
  if (a === null) return "";
  if (!a.available) return "· no agent running";
  if (a.keys.length === 0) return "· agent holds no keys (ssh-add)";
  return `· ${a.keys.length} key${a.keys.length === 1 ? "" : "s"}`;
}

function Label({ children }: { children: string }) {
  return <span className="font-condensed uppercase text-eyebrow text-text-lo">{children}</span>;
}

function Input({
  value,
  onChange,
  placeholder,
  password,
  autoFocus,
  required,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  password?: boolean;
  autoFocus?: boolean;
  required?: boolean;
}) {
  return (
    <input
      type={password ? "password" : "text"}
      value={value}
      required={required}
      autoFocus={autoFocus}
      spellCheck={false}
      // Never offer to remember a credential: the browser password manager is
      // not where an ops tool's secrets belong.
      autoComplete="off"
      onChange={(e) => onChange(e.target.value)}
      {...(placeholder ? { placeholder } : {})}
      className="num border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                 text-text-hi placeholder:text-text-lo focus:border-amber-500 focus:outline-none"
    />
  );
}
