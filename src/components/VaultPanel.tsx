import { useCallback, useEffect, useState } from "react";
import {
  vaultAudit,
  vaultChangePassword,
  vaultCreate,
  vaultList,
  vaultLock,
  vaultRemove,
  vaultSet,
  vaultStatus,
  vaultUnlock,
  type AuditView,
  type VaultStatus,
} from "@/lib/vault";

/**
 * The vault — CLAUDE.md §12.3.
 *
 * Three states, and the panel is really three panels:
 *
 *   no vault.bin  → create, with the portability warning stated up front
 *   locked        → unlock
 *   unlocked      → the record list, plus the audit log
 *
 * The record list shows **ids only**. There is no reveal button, no copy
 * button, and no eye icon, because the backend exposes no command that could
 * feed one. That absence is the design, so it is worth stating in the UI rather
 * than leaving the user hunting for a control that was never there.
 */
export function VaultPanel({ onClose }: { onClose: () => void }) {
  const [status, setStatus] = useState<VaultStatus | null>(null);
  const [ids, setIds] = useState<string[]>([]);
  const [audit, setAudit] = useState<AuditView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showAudit, setShowAudit] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const s = await vaultStatus();
      setStatus(s);
      setIds(s.unlocked ? await vaultList() : []);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // The backend auto-locks after an idle timeout. Without a poll the panel
  // would keep showing an unlocked vault that no longer is, and every action
  // would fail with "the vault is locked" for no visible reason.
  useEffect(() => {
    const t = window.setInterval(() => void refresh(), 30_000);
    return () => window.clearInterval(t);
  }, [refresh]);

  const run = async (f: () => Promise<unknown>) => {
    setError(null);
    setBusy(true);
    try {
      await f();
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="absolute inset-0 z-50 flex items-center justify-center bg-chassis-900/80">
      <div className="flex max-h-[80vh] w-[560px] flex-col border-hairline border-bezel-500 bg-chassis-800">
        <header className="flex items-center justify-between border-b-hairline border-bezel-500 px-4 py-2">
          <p className="font-condensed uppercase text-eyebrow text-text-lo">vault</p>
          <div className="flex items-center gap-3">
            {status?.unlocked && (
              <button
                onClick={() => void run(async () => void (await vaultLock()))}
                className="text-micro text-text-lo hover:text-text-hi"
              >
                lock now
              </button>
            )}
            <button onClick={onClose} className="text-micro text-text-lo hover:text-text-hi">
              close
            </button>
          </div>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          {error && (
            <p className="mb-3 border-hairline border-red-500 p-2 text-micro text-red-500">
              {error}
            </p>
          )}

          {!status && <p className="text-small text-text-lo">Checking…</p>}

          {status && !status.exists && <Create busy={busy} run={run} />}
          {status && status.exists && !status.unlocked && <Unlock busy={busy} run={run} />}
          {status && status.unlocked && (
            <Unlocked
              ids={ids}
              busy={busy}
              run={run}
              status={status}
              showAudit={showAudit}
              audit={audit}
              onToggleAudit={async () => {
                if (!showAudit) setAudit(await vaultAudit());
                setShowAudit((v) => !v);
              }}
            />
          )}
        </div>

        {status && (
          <footer className="border-t-hairline border-bezel-500 px-4 py-2">
            <p className="truncate text-micro text-text-lo">
              <span className="num">{status.path}</span>
            </p>
          </footer>
        )}
      </div>
    </div>
  );
}

function Create({
  busy,
  run,
}: {
  busy: boolean;
  run: (f: () => Promise<unknown>) => Promise<void>;
}) {
  const [pw, setPw] = useState("");
  const [again, setAgain] = useState("");
  const mismatch = again.length > 0 && pw !== again;

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void run(async () => {
          await vaultCreate(pw);
          setPw("");
          setAgain("");
        });
      }}
    >
      <p className="mb-2 text-small text-text-hi">Create a vault</p>

      {/* The one thing the user must understand before choosing a password.
          §12.3 calls a mistake here unrecoverable, so it is stated before the
          field, not in a tooltip after it. */}
      <p className="mb-3 border-hairline border-amber-500 p-2 text-micro text-text-mid">
        This password is the <span className="text-amber-500">only</span> way in. It is not stored
        anywhere, not backed by Windows Hello or your keychain, and cannot be reset. That is
        deliberate: it is also why copying <span className="num">vault.bin</span> to another machine
        and typing this password is all you need to recover.
      </p>

      <Field label="master password" value={pw} onChange={setPw} autoFocus />
      <Field label="repeat" value={again} onChange={setAgain} />
      {mismatch && <p className="mt-1 text-micro text-red-500">The passwords do not match.</p>}

      <button
        type="submit"
        disabled={busy || pw.length < 8 || mismatch || !again}
        className="mt-3 border-hairline border-amber-500 bg-amber-900 px-3 py-1 text-small
                   text-amber-500 disabled:opacity-40"
      >
        {busy ? "Creating…" : "Create vault"}
      </button>
      {pw.length > 0 && pw.length < 8 && (
        <p className="mt-1 text-micro text-text-lo">At least 8 characters.</p>
      )}
    </form>
  );
}

function Unlock({
  busy,
  run,
}: {
  busy: boolean;
  run: (f: () => Promise<unknown>) => Promise<void>;
}) {
  const [pw, setPw] = useState("");
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void run(async () => {
          await vaultUnlock(pw);
          setPw("");
        });
      }}
    >
      <p className="mb-3 text-small text-text-hi">Unlock the vault</p>
      <Field label="master password" value={pw} onChange={setPw} autoFocus />
      <button
        type="submit"
        disabled={busy || !pw}
        className="mt-3 border-hairline border-amber-500 bg-amber-900 px-3 py-1 text-small
                   text-amber-500 disabled:opacity-40"
      >
        {busy ? "Unlocking…" : "Unlock"}
      </button>
    </form>
  );
}

function Unlocked({
  ids,
  busy,
  run,
  status,
  showAudit,
  audit,
  onToggleAudit,
}: {
  ids: string[];
  busy: boolean;
  run: (f: () => Promise<unknown>) => Promise<void>;
  status: VaultStatus;
  showAudit: boolean;
  audit: AuditView | null;
  onToggleAudit: () => Promise<void>;
}) {
  const [newId, setNewId] = useState("");
  const [newSecret, setNewSecret] = useState("");
  const [changing, setChanging] = useState(false);
  const [newMaster, setNewMaster] = useState("");

  return (
    <>
      <div className="mb-3 flex items-baseline justify-between">
        <p className="text-small text-text-hi">
          {ids.length} {ids.length === 1 ? "secret" : "secrets"}
        </p>
        <p className="text-micro text-text-lo">
          auto-locks after <span className="num">{Math.round(status.idleTimeoutSecs / 60)}</span> min
          idle
        </p>
      </div>

      {ids.length === 0 && (
        <p className="mb-3 text-micro text-text-lo">
          Nothing stored yet. Add one below, then reference it from a host as{" "}
          <span className="num">vault:&lt;id&gt;</span>.
        </p>
      )}

      <ul className="mb-3">
        {ids.map((id) => (
          <li
            key={id}
            className="flex items-center justify-between border-b-hairline border-bezel-500 py-1"
          >
            <span className="num truncate text-small text-text-hi">{id}</span>
            <div className="flex shrink-0 items-center gap-3">
              <span className="text-micro text-text-lo">vault:{id}</span>
              <button
                disabled={busy}
                onClick={() => void run(() => vaultRemove(id))}
                className="text-micro text-text-lo hover:text-red-500"
              >
                delete
              </button>
            </div>
          </li>
        ))}
      </ul>

      {/* Stated because its absence looks like a missing feature otherwise. */}
      <p className="mb-4 text-micro text-text-lo">
        Secrets cannot be shown. The app exposes no command that returns one — connecting resolves
        the reference natively, so the plaintext never reaches this window.
      </p>

      <form
        className="border-t-hairline border-bezel-500 pt-3"
        onSubmit={(e) => {
          e.preventDefault();
          void run(async () => {
            await vaultSet(newId.trim(), newSecret);
            setNewId("");
            setNewSecret("");
          });
        }}
      >
        <p className="mb-2 font-condensed uppercase text-eyebrow text-text-lo">add or replace</p>
        <div className="grid grid-cols-[88px_1fr] items-center gap-x-2 gap-y-2">
          <Label>id</Label>
          <input
            value={newId}
            onChange={(e) => setNewId(e.target.value)}
            placeholder="prod-web-01/deploy"
            spellCheck={false}
            className="num border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                       text-text-hi placeholder:text-text-lo focus:border-amber-500 focus:outline-none"
          />
          <Label>secret</Label>
          <input
            type="password"
            value={newSecret}
            onChange={(e) => setNewSecret(e.target.value)}
            autoComplete="off"
            className="num border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                       text-text-hi focus:border-amber-500 focus:outline-none"
          />
        </div>
        {ids.includes(newId.trim()) && (
          <p className="mt-1 text-micro text-amber-500">
            <span className="num">{newId.trim()}</span> already exists and will be overwritten.
          </p>
        )}
        <button
          type="submit"
          disabled={busy || !newId.trim() || !newSecret}
          className="mt-2 border-hairline border-bezel-500 px-3 py-1 text-small text-text-mid
                     hover:border-text-lo disabled:opacity-40"
        >
          Save
        </button>
      </form>

      <div className="mt-4 flex gap-4 border-t-hairline border-bezel-500 pt-3">
        <button
          onClick={() => setChanging((v) => !v)}
          className="text-micro text-text-lo hover:text-text-hi"
        >
          change master password
        </button>
        <button
          onClick={() => void onToggleAudit()}
          className="text-micro text-text-lo hover:text-text-hi"
        >
          {showAudit ? "hide audit log" : "audit log"}
        </button>
      </div>

      {changing && (
        <form
          className="mt-2"
          onSubmit={(e) => {
            e.preventDefault();
            void run(async () => {
              await vaultChangePassword(newMaster);
              setNewMaster("");
              setChanging(false);
            });
          }}
        >
          <Field label="new master" value={newMaster} onChange={setNewMaster} autoFocus />
          <p className="mt-1 text-micro text-text-lo">
            Every secret is re-encrypted under the new password. The old one stops working
            immediately, on every machine holding a copy of this file.
          </p>
          <button
            type="submit"
            disabled={busy || newMaster.length < 8}
            className="mt-2 border-hairline border-amber-500 px-3 py-1 text-small text-amber-500
                       disabled:opacity-40"
          >
            Re-encrypt
          </button>
        </form>
      )}

      {showAudit && audit && (
        <div className="mt-3 border-t-hairline border-bezel-500 pt-2">
          {audit.brokenAt !== null && (
            <p className="mb-2 border-hairline border-red-500 p-2 text-micro text-red-500">
              The hash chain breaks at entry <span className="num">{audit.brokenAt}</span>. The log
              was edited outside OpsPilot — entries at or after that point cannot be trusted.
            </p>
          )}
          {audit.entries.length === 0 ? (
            <p className="text-micro text-text-lo">No entries.</p>
          ) : (
            <ul className="max-h-48 overflow-y-auto">
              {audit.entries
                .slice()
                .reverse()
                .map((e) => (
                  <li key={e.seq} className="flex gap-2 py-[1px] text-micro">
                    <span className="num w-8 shrink-0 text-right text-text-lo">{e.seq}</span>
                    <span className="w-24 shrink-0 text-text-mid">{e.action}</span>
                    <span className="num truncate text-text-lo">{e.id}</span>
                  </li>
                ))}
            </ul>
          )}
        </div>
      )}
    </>
  );
}

function Label({ children }: { children: string }) {
  return <span className="font-condensed uppercase text-eyebrow text-text-lo">{children}</span>;
}

function Field({
  label,
  value,
  onChange,
  autoFocus,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  autoFocus?: boolean;
}) {
  return (
    <div className="grid grid-cols-[112px_1fr] items-center gap-x-2 py-[2px]">
      <Label>{label}</Label>
      <input
        type="password"
        value={value}
        autoFocus={autoFocus}
        autoComplete="off"
        onChange={(e) => onChange(e.target.value)}
        className="num border-hairline border-bezel-500 bg-chassis-900 px-2 py-1 text-small
                   text-text-hi focus:border-amber-500 focus:outline-none"
      />
    </div>
  );
}
