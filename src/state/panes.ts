import { create } from "zustand";
import { release } from "./terminals";

/**
 * Split layout — UI_BUILD_GUIDE Step 9.
 *
 * "Model the layout as a binary tree of panes with split ratios."
 *
 * A leaf is one terminal. A branch splits its area between two children at a
 * ratio. That is the whole model: every operation below is a tree edit, which is
 * why closing a pane can collapse a branch without any special cases.
 *
 * Each tab owns its own tree, so splitting in one tab never disturbs another.
 */

export type Direction = "horizontal" | "vertical";

export type Node =
  | { type: "leaf"; id: string }
  | {
      type: "branch";
      id: string;
      /** "vertical" splits left|right; "horizontal" splits top/bottom. */
      direction: Direction;
      /** Fraction of the area given to `a`, clamped to keep both usable. */
      ratio: number;
      a: Node;
      b: Node;
    };

const MIN_RATIO = 0.1;
const MAX_RATIO = 0.9;

let counter = 0;
const nextId = () => `pane-${++counter}`;

export function newLeaf(): Node {
  return { type: "leaf", id: nextId() };
}

/** Every leaf id, left-to-right / top-to-bottom. */
export function leaves(node: Node): string[] {
  return node.type === "leaf" ? [node.id] : [...leaves(node.a), ...leaves(node.b)];
}

export function countLeaves(node: Node): number {
  return node.type === "leaf" ? 1 : countLeaves(node.a) + countLeaves(node.b);
}

/** Replace the leaf `id` with a branch containing it and a new sibling. */
export function split(node: Node, id: string, direction: Direction): { tree: Node; created: string } {
  let created = "";
  const walk = (n: Node): Node => {
    if (n.type === "leaf") {
      if (n.id !== id) return n;
      const sibling = newLeaf();
      created = sibling.id;
      return { type: "branch", id: nextId(), direction, ratio: 0.5, a: n, b: sibling };
    }
    return { ...n, a: walk(n.a), b: walk(n.b) };
  };
  return { tree: walk(node), created };
}

/**
 * Remove a leaf, collapsing its parent branch into the surviving sibling.
 *
 * Returns null when the last pane is closed — the caller closes the tab, because
 * a tab with zero panes is not a state worth representing.
 */
export function closePane(node: Node, id: string): Node | null {
  if (node.type === "leaf") return node.id === id ? null : node;
  const a = closePane(node.a, id);
  const b = closePane(node.b, id);
  if (a === null) return b;
  if (b === null) return a;
  return { ...node, a, b };
}

export function setRatio(node: Node, branchId: string, ratio: number): Node {
  if (node.type === "leaf") return node;
  if (node.id === branchId) {
    return { ...node, ratio: Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio)) };
  }
  return { ...node, a: setRatio(node.a, branchId, ratio), b: setRatio(node.b, branchId, ratio) };
}

/** Next/previous leaf in document order — what Ctrl+Alt+arrow moves through. */
export function neighbour(node: Node, id: string, delta: number): string | null {
  const list = leaves(node);
  const i = list.indexOf(id);
  if (i < 0) return null;
  const next = i + delta;
  return list[next] ?? null;
}

interface PaneState {
  /** One tree per tab id. */
  trees: Record<string, Node>;
  /** Focused leaf per tab. */
  focused: Record<string, string>;
  /**
   * Broadcast: keystrokes go to every pane in the tab at once.
   *
   * Step 9 is emphatic about this being unmistakable, and the reason is stated
   * plainly: "Typing the same command into four production servers by accident
   * is the worst thing this app could let a user do." So it is per-tab, off by
   * default, never persisted, and cleared whenever the pane set changes.
   */
  broadcast: Record<string, boolean>;

  ensure: (tabId: string) => Node;
  splitFocused: (tabId: string, direction: Direction) => string | null;
  close: (tabId: string, paneId: string) => void;
  focus: (tabId: string, paneId: string) => void;
  focusNeighbour: (tabId: string, delta: number) => void;
  resize: (tabId: string, branchId: string, ratio: number) => void;
  toggleBroadcast: (tabId: string) => void;
  forget: (tabId: string) => void;
}

export const usePanes = create<PaneState>((set, get) => ({
  trees: {},
  focused: {},
  broadcast: {},

  ensure: (tabId) => {
    const existing = get().trees[tabId];
    if (existing) return existing;
    const leaf = newLeaf();
    set((s) => ({
      trees: { ...s.trees, [tabId]: leaf },
      focused: { ...s.focused, [tabId]: leaf.id },
    }));
    return leaf;
  },

  splitFocused: (tabId, direction) => {
    const tree = get().trees[tabId];
    const focusedId = get().focused[tabId];
    if (!tree || !focusedId) return null;
    const { tree: next, created } = split(tree, focusedId, direction);
    set((s) => ({
      trees: { ...s.trees, [tabId]: next },
      focused: { ...s.focused, [tabId]: created },
      // The pane set changed, so an armed broadcast now covers panes the user
      // never opted into. Disarm rather than silently widen the blast radius.
      broadcast: { ...s.broadcast, [tabId]: false },
    }));
    return created;
  },

  close: (tabId, paneId) => {
    const tree = get().trees[tabId];
    if (!tree) return;
    // The pane is gone, so is its session (state/terminals.ts).
    release(paneId);
    const next = closePane(tree, paneId);
    set((s) => {
      const trees = { ...s.trees };
      const focused = { ...s.focused };
      if (next === null) {
        delete trees[tabId];
        delete focused[tabId];
      } else {
        trees[tabId] = next;
        const list = leaves(next);
        if (!list.includes(focused[tabId] ?? "")) {
          focused[tabId] = list[0] ?? "";
        }
      }
      return { trees, focused, broadcast: { ...s.broadcast, [tabId]: false } };
    });
  },

  focus: (tabId, paneId) => set((s) => ({ focused: { ...s.focused, [tabId]: paneId } })),

  focusNeighbour: (tabId, delta) => {
    const tree = get().trees[tabId];
    const current = get().focused[tabId];
    if (!tree || !current) return;
    const next = neighbour(tree, current, delta);
    if (next) set((s) => ({ focused: { ...s.focused, [tabId]: next } }));
  },

  resize: (tabId, branchId, ratio) => {
    const tree = get().trees[tabId];
    if (!tree) return;
    set((s) => ({ trees: { ...s.trees, [tabId]: setRatio(tree, branchId, ratio) } }));
  },

  toggleBroadcast: (tabId) =>
    set((s) => ({ broadcast: { ...s.broadcast, [tabId]: !s.broadcast[tabId] } })),

  forget: (tabId) =>
    set((s) => {
      // Every pane of the tab ends with it.
      const tree = s.trees[tabId];
      if (tree) for (const id of leaves(tree)) release(id);
      const trees = { ...s.trees };
      const focused = { ...s.focused };
      const broadcast = { ...s.broadcast };
      delete trees[tabId];
      delete focused[tabId];
      delete broadcast[tabId];
      return { trees, focused, broadcast };
    }),
}));
