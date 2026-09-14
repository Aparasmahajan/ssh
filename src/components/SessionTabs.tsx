import { useEffect, useState } from "react";
import { useSessions } from "@/state/sessions";
import { hasMod } from "@/lib/platform";

/**
 * The session tab strip — UI_BUILD_GUIDE Step 6.
 *
 * Built: middle-click closes, ⌘1–9 jumps, ⌘W closes, ⌘⇧[ / ⌘⇧] cycle, and
 * drag-to-reorder (also ⌘⇧PageUp/PageDown, because §0.8 requires every action to
 * be reachable without a mouse and a drag handle is the easiest place to forget
 * that).
 *
 * Not built: the LinkStrip sparkline (§0.4), which needs per-session RTT that
 * only exists once sessions report it. Recorded in COMPLETED.md rather than
 * faked with random numbers.
 */
export function SessionTabs({ onNew }: { onNew: () => void }) {
  const { tabs, activeId, activate, close, cycle, activateIndex, move } = useSessions();
  // The tab under the pointer during a drag. Only used to dim it; the reorder
  // itself happens live on drag-over, so the strip always shows where the tab
  // will land rather than promising it with a separate indicator.
  const [dragging, setDragging] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!hasMod(e)) return;

      // Move the active tab. PageUp/PageDown rather than the arrows: the arrows
      // are cursor keys the shell wants, and Ctrl+Shift+PageUp is the same
      // "move this tab" chord VS Code and Chrome use.
      if (e.shiftKey && (e.key === "PageUp" || e.key === "PageDown")) {
        const { tabs: ts, activeId: id } = useSessions.getState();
        const i = ts.findIndex((t) => t.id === id);
        if (i >= 0 && id) move(id, i + (e.key === "PageDown" ? 1 : -1));
        e.preventDefault();
        e.stopPropagation();
        return;
      }

      if (e.shiftKey && (e.key === "[" || e.key === "]")) {
        cycle(e.key === "]" ? 1 : -1);
        e.preventDefault();
        return;
      }
      if (e.key.toLowerCase() === "w") {
        const { activeId: id } = useSessions.getState();
        if (id) close(id);
        e.preventDefault();
        return;
      }
      if (/^[1-9]$/.test(e.key)) {
        activateIndex(Number(e.key));
        e.preventDefault();
      }
    };
    // Capture phase: a focused terminal wants nearly every key, but not these.
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [activateIndex, close, cycle, move]);

  return (
    <div
      role="tablist"
      aria-label="Sessions"
      className="flex h-tabstrip shrink-0 items-stretch gap-[1px] overflow-x-auto border-b-hairline border-bezel-500 bg-chassis-900"
    >
      {tabs.map((t, index) => {
        const isActive = t.id === activeId;
        return (
          <div
            key={t.id}
            role="tab"
            aria-selected={isActive}
            tabIndex={0}
            draggable
            onDragStart={(e) => {
              setDragging(t.id);
              e.dataTransfer.effectAllowed = "move";
              // Firefox refuses to start a drag without payload; the id is also
              // the only thing a drop needs.
              e.dataTransfer.setData("text/plain", t.id);
            }}
            onDragEnd={() => setDragging(null)}
            onDragOver={(e) => {
              if (!dragging || dragging === t.id) return;
              // Reorder as the pointer crosses each tab, so the strip is always
              // showing the result rather than promising it.
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              move(dragging, index);
            }}
            onDrop={(e) => {
              e.preventDefault();
              setDragging(null);
            }}
            onClick={() => activate(t.id)}
            onKeyDown={(e) => e.key === "Enter" && activate(t.id)}
            onAuxClick={(e) => {
              // Middle-click closes, the convention everywhere else.
              if (e.button === 1) {
                e.preventDefault();
                close(t.id);
              }
            }}
            className={`group relative flex min-w-[150px] max-w-[240px] cursor-default items-center
                        gap-2 px-3 ${isActive ? "bg-chassis-700" : "bg-chassis-800"}
                        ${dragging === t.id ? "opacity-50" : ""}`}
          >
            {/* 3px amber top edge marks the active tab — the same active-edge
                language as the module rail (§0.5). */}
            <span
              aria-hidden
              className={`absolute inset-x-0 top-0 h-[2px] ${isActive ? "bg-amber-500" : "bg-transparent"}`}
            />
            <span
              aria-hidden
              className={`h-[6px] w-[6px] shrink-0 rounded-full ${
                // Cyan is link state and nothing else (§0.2). A remote session
                // is link state; a local shell is not.
                t.connection ? "bg-cyan-400" : "bg-text-lo"
              }`}
            />
            <span
              className={`min-w-0 flex-1 truncate text-small ${
                isActive ? "text-text-hi" : "text-text-mid"
              }`}
              title={t.title}
            >
              {t.kind === "sftp" && <span className="text-text-lo">⇅ </span>}
              {t.title}
            </span>
            <button
              type="button"
              aria-label={`Close ${t.title}`}
              onClick={(e) => {
                e.stopPropagation();
                close(t.id);
              }}
              className="shrink-0 text-micro text-text-lo opacity-0 transition-opacity
                         duration-hover group-hover:opacity-100 hover:text-red-500
                         focus-visible:opacity-100"
            >
              ✕
            </button>
          </div>
        );
      })}

      <button
        type="button"
        onClick={onNew}
        aria-label="New session"
        className="flex w-9 shrink-0 items-center justify-center text-large text-text-lo
                   hover:text-amber-500"
      >
        +
      </button>
    </div>
  );
}
