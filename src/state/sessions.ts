import { create } from "zustand";
import type { ConnectionInfo } from "@/lib/remote";

/**
 * Open sessions — the tab model.
 *
 * A session is a shell (local or remote) or an SFTP browser. The Rust registry
 * already keys sessions by id and supports as many as you like; this is the
 * frontend's view of which ones exist and which is in front.
 *
 * Tabs deliberately hold no terminal state. The grid lives in Rust and is
 * fetched by generation, so switching tabs is a mount, not a transfer — and a
 * background tab keeps receiving output whether or not anything is looking at it.
 */

export type TabKind = "shell" | "sftp";

export interface Tab {
  /** Frontend id; stable across re-renders and independent of the Rust session id. */
  id: string;
  kind: TabKind;
  title: string;
  /** Shell id for a local session; absent for remote. */
  shellId?: string;
  /** Set when this tab is attached to an SSH connection. */
  connection?: ConnectionInfo;
}

interface SessionState {
  tabs: Tab[];
  activeId: string | null;

  openLocal: (shellId: string, title: string) => string;
  openRemote: (connection: ConnectionInfo, kind: TabKind) => string;
  close: (id: string) => void;
  activate: (id: string) => void;
  /** Cycle by ±1, wrapping. Drives Ctrl+Shift+[ / ]. */
  cycle: (delta: number) => void;
  /** 1-based, for Ctrl+1..9. */
  activateIndex: (n: number) => void;
  rename: (id: string, title: string) => void;
  /**
   * Move a tab to `toIndex`, clamped. Order is the model, so `Ctrl+1..9` and
   * the cycle bindings follow a reorder for free.
   */
  move: (id: string, toIndex: number) => void;
}

let counter = 0;
const nextId = () => `tab-${++counter}`;

export const useSessions = create<SessionState>((set, get) => ({
  tabs: [],
  activeId: null,

  openLocal: (shellId, title) => {
    const id = nextId();
    set((s) => ({ tabs: [...s.tabs, { id, kind: "shell", title, shellId }], activeId: id }));
    return id;
  },

  openRemote: (connection, kind) => {
    const id = nextId();
    const title =
      kind === "sftp"
        ? `sftp · ${connection.host}`
        : `${connection.user}@${connection.host}`;
    set((s) => ({ tabs: [...s.tabs, { id, kind, title, connection }], activeId: id }));
    return id;
  },

  close: (id) =>
    set((s) => {
      const idx = s.tabs.findIndex((t) => t.id === id);
      const tabs = s.tabs.filter((t) => t.id !== id);
      if (s.activeId !== id) return { tabs, activeId: s.activeId };
      // Focus the neighbour, preferring the one to the left — closing the last
      // tab should land you on the new last tab, not on nothing.
      const next = tabs[Math.min(idx, tabs.length - 1)];
      return { tabs, activeId: next?.id ?? null };
    }),

  activate: (id) => set({ activeId: id }),

  cycle: (delta) => {
    const { tabs, activeId } = get();
    if (tabs.length === 0) return;
    const i = tabs.findIndex((t) => t.id === activeId);
    const next = (((i < 0 ? 0 : i) + delta) % tabs.length + tabs.length) % tabs.length;
    set({ activeId: tabs[next]?.id ?? null });
  },

  activateIndex: (n) => {
    const { tabs } = get();
    const t = tabs[n - 1];
    if (t) set({ activeId: t.id });
  },

  rename: (id, title) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, title } : t)) })),

  move: (id, toIndex) =>
    set((s) => {
      const from = s.tabs.findIndex((t) => t.id === id);
      if (from < 0) return {};
      const to = Math.max(0, Math.min(toIndex, s.tabs.length - 1));
      if (to === from) return {};
      const tabs = s.tabs.slice();
      // Splice out first, then insert: `to` is the destination index in the
      // list *without* the dragged tab, which is what both the drag handler and
      // the keyboard shortcut compute.
      const [moved] = tabs.splice(from, 1);
      if (!moved) return {};
      tabs.splice(to, 0, moved);
      return { tabs };
    }),
}));
