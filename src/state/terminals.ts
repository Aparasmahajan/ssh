import { closeSession } from "@/lib/session";
import { registerPane, unregisterPane } from "@/lib/broadcast";

/**
 * Who owns a terminal session: the PANE, not the component drawing it.
 *
 * The component used to own it — open on mount, close on unmount — and React
 * unmounts components for reasons that have nothing to do with the user ending
 * a session:
 *
 *   · switching tabs (only the active tab's pane tree is mounted),
 *   · opening the Docker / Databases / Kubernetes module (they replace the
 *     surface),
 *   · splitting a pane (the original leaf moves one level deeper in the tree,
 *     and React remounts anything whose parent changes).
 *
 * Each of those killed the running shell and silently started a fresh one.
 * Now a session lives exactly as long as its pane: `acquire` attaches to the
 * existing one or opens it once; `release` is called only when the pane or its
 * tab is closed.
 *
 * Also fixed by construction: an open still in flight when its pane is closed
 * is closed the moment it resolves, instead of leaking a shell (or an SSH
 * channel, or a container exec) that nothing references.
 */

interface Entry {
  /** What was opened: `local:<shell>`, `ssh:<key>`, `stream:<json>`. A pane
   *  asked for a different key (the shell picker) gets a fresh session. */
  key: string;
  id: number | null;
  pending: Promise<number> | null;
  released: boolean;
}

const entries = new Map<string, Entry>();

/** The session for `paneId` opened with `key`, opening it if needed. */
export function acquire(paneId: string, key: string, open: () => Promise<number>): Promise<number> {
  const cur = entries.get(paneId);
  if (cur && !cur.released) {
    if (cur.key === key) {
      if (cur.id !== null) return Promise.resolve(cur.id);
      if (cur.pending) return cur.pending;
    } else {
      // Same pane, different target: the user picked another shell.
      release(paneId);
    }
  }

  const entry: Entry = { key, id: null, pending: null, released: false };
  entries.set(paneId, entry);
  entry.pending = open().then(
    (id) => {
      entry.pending = null;
      if (entry.released) {
        // Closed while opening: close it now, never leak it.
        void closeSession(id).catch(() => undefined);
        throw new Error("pane closed");
      }
      entry.id = id;
      registerPane(paneId, id);
      return id;
    },
    (e: unknown) => {
      // A failed open leaves nothing behind; the next attach retries.
      if (entries.get(paneId) === entry) entries.delete(paneId);
      throw e;
    },
  );
  return entry.pending;
}

/** The key this pane's session was opened with, if any — lets a remounted
 *  pane restore its shell picker instead of reopening the default shell. */
export function keyOf(paneId: string): string | null {
  const e = entries.get(paneId);
  return e && !e.released ? e.key : null;
}

/** End the pane's session. Idempotent. */
export function release(paneId: string): void {
  const e = entries.get(paneId);
  if (!e) return;
  e.released = true;
  entries.delete(paneId);
  unregisterPane(paneId);
  if (e.id !== null) void closeSession(e.id).catch(() => undefined);
  // A pending open is closed by its own continuation (see `acquire`).
}

/** Test/debug: how many panes currently own a session. */
export function liveCount(): number {
  return entries.size;
}
