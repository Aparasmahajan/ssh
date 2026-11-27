import { useCallback, useEffect, useRef } from "react";
import { usePanes, type Node } from "@/state/panes";
import { IS_APPLE, hasMod } from "@/lib/platform";
import type { ConnectionInfo } from "@/lib/remote";
import { TerminalSurface } from "./TerminalSurface";

/**
 * Renders a split layout — UI_BUILD_GUIDE Step 9.
 *
 * The tree is nested flex boxes rather than one surface with scissor rects.
 * Step 9 asks for "one wgpu surface, not one per pane", and that remains the
 * right shape — but there is no wgpu surface yet (docs/DECISIONS.md D-19), and
 * with a 2D canvas one element per pane is what the platform gives you. When the
 * renderer lands this component computes rects instead of nesting divs; nothing
 * above it changes.
 */
export function PaneTree({
  tabId,
  connection,
  onConnect,
  onCloseTab,
}: {
  tabId: string;
  connection: ConnectionInfo | null;
  onConnect: () => void;
  onCloseTab: () => void;
}) {
  const tree = usePanes((s) => s.trees[tabId]);
  const focused = usePanes((s) => s.focused[tabId]);
  const broadcast = usePanes((s) => s.broadcast[tabId] ?? false);
  const ensure = usePanes((s) => s.ensure);
  const splitFocused = usePanes((s) => s.splitFocused);
  const close = usePanes((s) => s.close);
  const focus = usePanes((s) => s.focus);
  const focusNeighbour = usePanes((s) => s.focusNeighbour);
  const toggleBroadcast = usePanes((s) => s.toggleBroadcast);

  useEffect(() => {
    ensure(tabId);
  }, [ensure, tabId]);

  // Closing the last pane closes the tab: a tab with no panes is not a state
  // worth representing.
  //
  // Only once a tree has EXISTED. On the first render `tree` is undefined
  // because `ensure` has not run yet, and both effects fire in the same commit:
  // `ensure` updates the store, but this effect still sees the render's stale
  // `undefined`. Testing `tree === undefined` alone closed every new tab the
  // instant it opened, so no terminal could ever be started.
  const hadTree = useRef(false);
  useEffect(() => {
    if (tree !== undefined) hadTree.current = true;
    else if (hadTree.current) onCloseTab();
  }, [tree, onCloseTab]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Split bindings differ by platform, and NOT for cosmetic reasons.
      //
      // Step 9 specifies ⌘D / ⌘⇧D. On macOS ⌘ is distinct from Ctrl, so that is
      // free. On Windows and Linux the same chord is Ctrl+D — which is EOF, the
      // way you exit a shell. Stealing it would break the terminal to add a
      // window-management shortcut, so those platforms use the Windows Terminal
      // convention instead: Alt+Shift+= and Alt+Shift+-.
      const splitKey = IS_APPLE
        ? hasMod(e) && !e.altKey && e.key.toLowerCase() === "d"
        : e.altKey && e.shiftKey && (e.key === "+" || e.key === "=" || e.key === "_" || e.key === "-");

      if (splitKey) {
        const horizontal = IS_APPLE
          ? e.shiftKey
          : e.key === "_" || e.key === "-";
        splitFocused(tabId, horizontal ? "horizontal" : "vertical");
        e.preventDefault();
        e.stopPropagation();
        return;
      }

      if (!hasMod(e)) return;

      // Ctrl/⌘+Shift+B — broadcast. No terminal meaning, so it is safe to take.
      if (e.shiftKey && e.key.toLowerCase() === "b") {
        toggleBroadcast(tabId);
        e.preventDefault();
        e.stopPropagation();
        return;
      }

      // Ctrl/⌘+Alt+arrows — move focus between panes.
      if (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        focusNeighbour(tabId, e.key === "ArrowRight" ? 1 : -1);
        e.preventDefault();
        e.stopPropagation();
      }
    };
    // Capture, and stopPropagation on every handled chord: preventDefault alone
    // does NOT stop the event reaching the terminal's own handler, so Ctrl+D
    // would have split a pane and sent EOF to the shell.
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [focusNeighbour, splitFocused, tabId, toggleBroadcast]);

  if (!tree) return null;

  return (
    <div className="relative flex min-h-0 flex-1 overflow-hidden">
      <Branch
        node={tree}
        tabId={tabId}
        focused={focused ?? ""}
        broadcast={broadcast}
        connection={connection}
        onConnect={onConnect}
        onFocus={(id) => focus(tabId, id)}
        onClose={(id) => close(tabId, id)}
      />
    </div>
  );
}

function Branch({
  node,
  tabId,
  focused,
  broadcast,
  connection,
  onConnect,
  onFocus,
  onClose,
}: {
  node: Node;
  tabId: string;
  focused: string;
  broadcast: boolean;
  connection: ConnectionInfo | null;
  onConnect: () => void;
  onFocus: (id: string) => void;
  onClose: (id: string) => void;
}) {
  const resize = usePanes((s) => s.resize);
  const boxRef = useRef<HTMLDivElement | null>(null);
  const dragging = useRef(false);

  const onPointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (!dragging.current || node.type !== "branch") return;
      const box = boxRef.current?.getBoundingClientRect();
      if (!box) return;
      const ratio =
        node.direction === "vertical"
          ? (e.clientX - box.left) / box.width
          : (e.clientY - box.top) / box.height;
      resize(tabId, node.id, ratio);
    },
    [node, resize, tabId],
  );

  if (node.type === "leaf") {
    const isFocused = node.id === focused;
    // Step 9: "2px amber border around every participating pane" in broadcast.
    // Focus alone gets a hairline, so the two states are never confusable.
    const border = broadcast
      ? "border-2 border-amber-500"
      : isFocused
        ? "border-hairline border-amber-500"
        : "border-hairline border-bezel-500";

    return (
      <div
        onMouseDown={() => onFocus(node.id)}
        className={`relative flex min-h-0 min-w-0 flex-1 overflow-hidden ${border}`}
      >
        <TerminalSurface
          key={node.id}
          active={isFocused}
          paneId={node.id}
          tabId={tabId}
          broadcast={broadcast}
          connection={connection}
          onConnect={onConnect}
          onDisconnect={() => onClose(node.id)}
        />
      </div>
    );
  }

  const vertical = node.direction === "vertical";
  return (
    <div
      ref={boxRef}
      className={`flex min-h-0 min-w-0 flex-1 ${vertical ? "flex-row" : "flex-col"}`}
      onPointerMove={onPointerMove}
      onPointerUp={() => {
        dragging.current = false;
      }}
    >
      <div className="flex min-h-0 min-w-0" style={{ flex: `${node.ratio} 1 0%` }}>
        <Branch
          node={node.a}
          tabId={tabId}
          focused={focused}
          broadcast={broadcast}
          connection={connection}
          onConnect={onConnect}
          onFocus={onFocus}
          onClose={onClose}
        />
      </div>

      {/* Same 4px visible / 12px hit-area rule as the shell's resizer, so the
          two behave identically under the pointer. */}
      <div
        role="separator"
        aria-orientation={vertical ? "vertical" : "horizontal"}
        onPointerDown={(e) => {
          dragging.current = true;
          e.currentTarget.setPointerCapture(e.pointerId);
        }}
        className={`relative shrink-0 bg-chassis-900 ${
          vertical ? "w-[4px] cursor-col-resize" : "h-[4px] cursor-row-resize"
        } hover:bg-bezel-500`}
      />

      <div className="flex min-h-0 min-w-0" style={{ flex: `${1 - node.ratio} 1 0%` }}>
        <Branch
          node={node.b}
          tabId={tabId}
          focused={focused}
          broadcast={broadcast}
          connection={connection}
          onConnect={onConnect}
          onFocus={onFocus}
          onClose={onClose}
        />
      </div>
    </div>
  );
}
