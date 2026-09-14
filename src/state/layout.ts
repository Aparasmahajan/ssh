import { create } from "zustand";
import { load, save } from "@/lib/persist";

export const MODULES = [
  { id: "workspace", glyph: "⌘", label: "Workspace" },
  { id: "sessions", glyph: "⇄", label: "Sessions" },
  { id: "docker", glyph: "▣", label: "Docker" },
  { id: "kubernetes", glyph: "⎈", label: "Kubernetes" },
  { id: "databases", glyph: "⛁", label: "Databases" },
  { id: "transfers", glyph: "⇅", label: "Transfers" },
  { id: "settings", glyph: "⚙", label: "Settings" },
] as const;

export type ModuleId = (typeof MODULES)[number]["id"];

/** UI_BUILD_GUIDE §0.5: tree is 260px, resizable 200–480. */
export const TREE_MIN = 200;
export const TREE_MAX = 480;
export const TREE_DEFAULT = 260;

/** §0.5: "Collapsed by default on screens under 1600px." */
const INSPECTOR_AUTOCOLLAPSE_WIDTH = 1600;

/** The slice that survives a restart. Panel sizes persist PER WORKSPACE. */
type Persisted = {
  treeWidth: number;
  treeCollapsed: boolean;
  inspectorCollapsed: boolean;
  activeModule: ModuleId;
};

type LayoutState = Persisted & {
  workspaceId: string;
  /** ⌘⏎ — the "I'm debugging, get out of my way" state. Deliberately NOT
   *  persisted: it is a momentary mode, and restoring into it on launch with
   *  no visible tree would read as a broken window. */
  zoomed: boolean;

  setActiveModule: (id: ModuleId) => void;
  setTreeWidth: (px: number) => void;
  toggleTree: () => void;
  toggleInspector: () => void;
  toggleZoom: () => void;
  switchWorkspace: (id: string) => void;
};

const clampTree = (px: number) => Math.min(TREE_MAX, Math.max(TREE_MIN, Math.round(px)));

function defaults(): Persisted {
  const narrow =
    typeof window !== "undefined" && window.innerWidth < INSPECTOR_AUTOCOLLAPSE_WIDTH;
  return {
    treeWidth: TREE_DEFAULT,
    treeCollapsed: false,
    inspectorCollapsed: narrow,
    activeModule: "workspace",
  };
}

const keyFor = (workspaceId: string) => `layout:${workspaceId}`;

function loadFor(workspaceId: string): Persisted {
  const d = defaults();
  const stored = load<Partial<Persisted>>(keyFor(workspaceId), {});
  return {
    treeWidth: clampTree(stored.treeWidth ?? d.treeWidth),
    treeCollapsed: stored.treeCollapsed ?? d.treeCollapsed,
    inspectorCollapsed: stored.inspectorCollapsed ?? d.inspectorCollapsed,
    activeModule: stored.activeModule ?? d.activeModule,
  };
}

const INITIAL_WORKSPACE = "production";

export const useLayout = create<LayoutState>((set, get) => {
  const persist = (next: Partial<Persisted>) => {
    const s = get();
    save(keyFor(s.workspaceId), {
      treeWidth: s.treeWidth,
      treeCollapsed: s.treeCollapsed,
      inspectorCollapsed: s.inspectorCollapsed,
      activeModule: s.activeModule,
      ...next,
    });
  };

  return {
    workspaceId: INITIAL_WORKSPACE,
    zoomed: false,
    ...loadFor(INITIAL_WORKSPACE),

    setActiveModule: (activeModule) => {
      set({ activeModule });
      persist({ activeModule });
    },
    setTreeWidth: (px) => {
      const treeWidth = clampTree(px);
      set({ treeWidth });
      persist({ treeWidth });
    },
    toggleTree: () => {
      const treeCollapsed = !get().treeCollapsed;
      set({ treeCollapsed });
      persist({ treeCollapsed });
    },
    toggleInspector: () => {
      const inspectorCollapsed = !get().inspectorCollapsed;
      set({ inspectorCollapsed });
      persist({ inspectorCollapsed });
    },
    toggleZoom: () => set({ zoomed: !get().zoomed }),
    switchWorkspace: (workspaceId) =>
      set({ workspaceId, zoomed: false, ...loadFor(workspaceId) }),
  };
});
