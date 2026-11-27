import { useCallback, useEffect, useState } from "react";
import { TREE_MAX, TREE_MIN, useLayout } from "@/state/layout";
import { useSessions } from "@/state/sessions";
import { CHORD, isAppChord } from "@/lib/platform";
import { isFontZoomKey, useSettings } from "@/state/settings";
import { IS_DESKTOP } from "@/lib/runtime";
import type { ConnectionInfo } from "@/lib/remote";
import {
  configGeneration,
  emptyHost,
  forwardAutoStart,
  hostDelete,
  hostUpsert,
  toUiHost,
  workspaceList,
  workspaceLoad,
  type AutoStarted,
  type StoredHost,
} from "@/lib/workspace";
import type { ConnectionKind, Host } from "@/types";

import { ModuleRail } from "./ModuleRail";
import { StatusRail } from "./StatusRail";
import { Resizer } from "./Resizer";
import { WorkspaceTree } from "./WorkspaceTree";
import { SessionSurfacePlaceholder } from "./Placeholders";
import { Inspector } from "./Inspector";
import { CommandPalette, type PaletteAction } from "./CommandPalette";
import { ConnectDialog } from "./ConnectDialog";
import { SftpBrowser } from "./SftpBrowser";
import { SessionTabs } from "./SessionTabs";
import { HostEditor } from "./HostEditor";
import { ForwardsPanel } from "./ForwardsPanel";
import { VaultPanel } from "./VaultPanel";
import { Landing } from "./Landing";
import { PaneTree } from "./PaneTree";
import { usePanes } from "@/state/panes";

/**
 * UI_BUILD_GUIDE §0.5 — the four-zone shell.
 *
 * The session surface now holds a tab strip over one of three surfaces —
 * terminal, SFTP, or forwards — chosen by the active tab and the module rail.
 * The tree is fed by the persisted workspace, not fixtures.
 */
export function AppShell({ fallbackHosts }: { fallbackHosts: readonly Host[] }) {
  const { treeWidth, treeCollapsed, inspectorCollapsed, zoomed, setTreeWidth, activeModule } =
    useLayout();
  const { tabs, activeId, openLocal, openRemote } = useSessions();

  const [workspace, setWorkspace] = useState<string>("");
  const [stored, setStored] = useState<StoredHost[]>([]);
  const [editing, setEditing] = useState<StoredHost | null>(null);
  const [connecting, setConnecting] = useState<{ prefill?: StoredHost; sftp?: boolean } | null>(
    null,
  );

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [vaultOpen, setVaultOpen] = useState(false);
  // Saved forwards that were meant to come up with a host and did not. Held
  // here rather than in the forwards panel because the user may never open that
  // module, and a tunnel they configured to start automatically failing in
  // silence is exactly the kind of thing this app must not do.
  const [forwardFailures, setForwardFailures] = useState<AutoStarted[]>([]);
  // Stable identity: the keybinding effect depends on this, and an inline arrow
  // would tear down and re-register the window listener on every render.
  const togglePalette = useCallback(() => setPaletteOpen((v) => !v), []);
  useShellKeybindings(togglePalette);

  /* ---- Load the persisted workspace ------------------------------------- */
  const reload = useCallback(async (name: string) => {
    const ws = await workspaceLoad(name);
    setStored(ws.host ?? []);
  }, []);

  useEffect(() => {
    if (!IS_DESKTOP) return;
    void (async () => {
      try {
        const names = await workspaceList();
        const first = names[0] ?? "default";
        setWorkspace(first);
        await reload(first);
      } catch {
        // A broken config must not black-hole the app; the tree falls back to
        // the demo hosts and the user can still open a local shell.
        setStored([]);
      }
    })();
  }, [reload]);

  /* ---- Hot reload (§12.2) ------------------------------------------------ */
  //
  // "Editing production.toml in Vim makes hosts appear in the tree." Polled at
  // 1 Hz rather than pushed: a human editing a file cannot tell 1 Hz from
  // instant, one save produces several filesystem events, and a counter
  // collapses them into a single reparse.
  useEffect(() => {
    if (!IS_DESKTOP || !workspace) return;
    let seen = -1;
    let live = true;
    const tick = async () => {
      try {
        const g = await configGeneration();
        if (!live || g === seen) return;
        // The first reading only establishes the baseline; reloading on it
        // would reparse the workspace we have just loaded.
        const first = seen < 0;
        seen = g;
        if (!first) await reload(workspace);
      } catch {
        // A watcher that cannot start is "no hot reload", not an error to show.
      }
    };
    void tick();
    const t = window.setInterval(() => void tick(), 1000);
    return () => {
      live = false;
      window.clearInterval(t);
    };
  }, [workspace, reload]);

  // Hosts come from disk when there are any; the bundled demo set is only a
  // first-run illustration, never mixed in with real ones.
  const hosts: readonly Host[] =
    stored.length > 0 ? stored.map(toUiHost) : IS_DESKTOP ? [] : fallbackHosts;

  const activeTab = tabs.find((t) => t.id === activeId) ?? null;
  const activeConnection: ConnectionInfo | null = activeTab?.connection ?? null;

  const saveHost = async (h: StoredHost) => {
    const ws = await hostUpsert(workspace || "default", h);
    setStored(ws.host ?? []);
    setEditing(null);
  };

  const removeHost = async (id: string) => {
    const ws = await hostDelete(workspace || "default", id);
    setStored(ws.host ?? []);
    setEditing(null);
  };

  /** Narrow a stored host to one identity, so the dialog prefills from it. */
  const pick = (h: StoredHost, identityName?: string): StoredHost =>
    identityName
      ? { ...h, identities: h.identities.filter((i) => i.name === identityName) }
      : h;

  /** A port chip was clicked: connect, then open the surface it names. */
  const openConnection = (host: Host, kind: ConnectionKind, identityName?: string) => {
    const source = stored.find((h) => h.id === host.id);
    if (!source) return;
    // Always via the dialog: the stored host holds no password, and a key may
    // need a passphrase. Prefilled, so it is one click plus whatever secret.
    setConnecting({ prefill: pick(source, identityName), sftp: kind === "SFTP" });
  };

  const runPaletteAction = (a: PaletteAction) => {
    switch (a.kind) {
      case "open-shell":
        setConnecting({ prefill: pick(a.host, a.identity) });
        break;
      case "open-sftp":
        setConnecting({ prefill: pick(a.host, a.identity), sftp: true });
        break;
      case "local-shell":
        openLocal("", "local shell");
        break;
      case "new-host":
        setEditing(emptyHost());
        break;
      case "edit-host":
        setEditing(a.host);
        break;
      case "module":
        useLayout.getState().setActiveModule(a.module as never);
        break;
      case "vault":
        setVaultOpen(true);
        break;
      case "font": {
        const st = useSettings.getState();
        if (a.delta === "reset") st.resetFont();
        else st.adjustFont(a.delta);
        break;
      }
    }
  };

  const surface = () => {
    if (!IS_DESKTOP) return <SessionSurfacePlaceholder />;
    if (activeModule === "transfers") return <ForwardsPanel connection={activeConnection} />;
    if (activeTab?.kind === "sftp" && activeTab.connection) {
      return <SftpBrowser connection={activeTab.connection} />;
    }
    if (!activeTab) {
      // Nothing running: show what CAN be run rather than an empty rectangle.
      return (
        <Landing
          hosts={stored}
          onLocalShell={() => openLocal("", "local shell")}
          onNewHost={() => setEditing(emptyHost())}
          onEditHost={setEditing}
          onOpenShell={(h, name) => setConnecting({ prefill: pick(h, name) })}
          onOpenSftp={(h, name) => setConnecting({ prefill: pick(h, name), sftp: true })}
        />
      );
    }
    // Each tab owns a split tree; an unsplit tab is a tree with one leaf, so
    // there is no separate "single pane" path to keep in sync.
    return (
      <PaneTree
        key={activeTab.id}
        tabId={activeTab.id}
        connection={activeTab.connection ?? null}
        onConnect={() => setConnecting({})}
        onCloseTab={() => {
          usePanes.getState().forget(activeTab.id);
          useSessions.getState().close(activeTab.id);
        }}
      />
    );
  };

  return (
    <div
      data-testid="app-shell"
      className="relative grid h-full w-full overflow-hidden bg-chassis-900"
      style={{ gridTemplateRows: "1fr var(--status-h)" }}
    >
      <div className="flex min-h-0 overflow-hidden">
        {!zoomed && <ModuleRail />}

        {!zoomed && !treeCollapsed && (
          <>
            <WorkspaceTree
              hosts={hosts}
              width={treeWidth}
              onOpenConnection={openConnection}
              onAddHost={() => setEditing(emptyHost())}
              onEditHost={(id) => {
                const h = stored.find((s) => s.id === id);
                if (h) setEditing(h);
              }}
            />
            <Resizer
              value={treeWidth}
              min={TREE_MIN}
              max={TREE_MAX}
              onChange={setTreeWidth}
              side="left"
              label="Resize workspace tree"
            />
          </>
        )}

        <section className="flex min-w-0 flex-1 flex-col overflow-hidden">
          {IS_DESKTOP && <SessionTabs onNew={() => setConnecting({})} />}
          {forwardFailures.length > 0 && (
            <div
              role="status"
              className="flex items-start gap-2 border-b-hairline border-amber-500 bg-amber-900/40
                         px-3 py-1 text-micro text-amber-500"
            >
              <span className="font-condensed uppercase text-eyebrow">forward not started</span>
              <span className="num min-w-0 flex-1 truncate">
                {forwardFailures.map((f) => `${f.name}: ${f.error ?? "unknown"}`).join(" · ")}
              </span>
              <button
                type="button"
                onClick={() => setForwardFailures([])}
                className="shrink-0 text-text-lo hover:text-text-hi"
                aria-label="Dismiss"
              >
                ×
              </button>
            </div>
          )}
          {surface()}
        </section>

        {!zoomed && !inspectorCollapsed &&
          (IS_DESKTOP ? <Inspector connection={activeConnection} /> : null)}
      </div>

      <StatusRail onOpenVault={() => setVaultOpen(true)} />

      {vaultOpen && <VaultPanel onClose={() => setVaultOpen(false)} />}

      {/* Mounted once and toggled, per §0.7's "must open in under 30ms. Mount it
          once, hidden... Do not mount on demand." */}
      <CommandPalette
        open={paletteOpen}
        hosts={stored}
        onClose={() => setPaletteOpen(false)}
        onAction={runPaletteAction}
      />

      {connecting && (
        <ConnectDialog
          {...(connecting.prefill ? { prefill: connecting.prefill } : {})}
          {...(connecting.sftp ? { defaultSftp: true } : {})}
          onCancel={() => setConnecting(null)}
          onConnected={(c, alsoSftp) => {
            openRemote(c, alsoSftp ? "sftp" : "shell");
            // Saved forwards flagged auto_start come up with the host. Fired
            // after the session is open, never awaited before it: the shell is
            // what was asked for, and a tunnel must not delay or block it.
            const hostId = connecting.prefill?.id;
            if (hostId) {
              void forwardAutoStart(c.key, workspace || "default", hostId)
                .then((rs) => setForwardFailures(rs.filter((r) => !r.started)))
                .catch(() => undefined);
            }
            setConnecting(null);
          }}
        />
      )}

      {editing && (
        <HostEditor
          host={editing}
          onSave={(h) => void saveHost(h)}
          onCancel={() => setEditing(null)}
          {...(stored.some((s) => s.id === editing.id)
            ? { onDelete: () => void removeHost(editing.id) }
            : {})}
        />
      )}
    </div>
  );
}

function useShellKeybindings(togglePalette: () => void) {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // Font zoom works anywhere, not only with the terminal focused — the
      // whole point is that it is reachable when you cannot read the screen.
      const zoom = isFontZoomKey(e);
      if (zoom) {
        const st = useSettings.getState();
        if (zoom === "in") st.adjustFont(1);
        else if (zoom === "out") st.adjustFont(-1);
        else st.resetFont();
        // Stop here: the terminal must not see it too. It used to handle zoom
        // itself as well, so each press inside a terminal changed size twice.
        e.preventDefault();
        e.stopPropagation();
        return;
      }

      const s = useLayout.getState();
      // Escape leaves zoom — except from inside a terminal, where Escape is
      // vim's most-pressed key and must not also unzoom the pane every time.
      // From there, the zoom chord toggles it back.
      if (e.key === "Escape" && s.zoomed && !inTerminal(e.target)) {
        s.toggleZoom();
        e.preventDefault();
        return;
      }
      // Ctrl+Shift on Linux/Windows, ⌘ on macOS — see `isAppChord` for why
      // plain Ctrl cannot be used: Ctrl+K, Ctrl+B and Ctrl+I are the shell's.
      if (isAppChord(e, CHORD.palette)) togglePalette();
      else if (isAppChord(e, CHORD.tree)) s.toggleTree();
      else if (isAppChord(e, CHORD.inspector)) s.toggleInspector();
      else if (isAppChord(e, CHORD.zoom)) s.toggleZoom();
      else return;

      // stopPropagation as well: a focused terminal would otherwise also
      // receive Ctrl+Shift+K and send 0x0B (kill-line) to the shell.
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [togglePalette]);
}

/** Is this event target inside a terminal surface? */
function inTerminal(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest('[aria-label="Terminal"]') !== null;
}
