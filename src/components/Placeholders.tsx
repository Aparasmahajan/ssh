/**
 * Zone stand-ins so the shell can be verified before its contents exist.
 * Each is replaced by a real phase:
 *   WorkspaceTree  → DONE, Phase 3 (Steps 4–5)
 *   SessionSurface → Phase 4 (Step 6 tab strip) and Phase 6 (Step 8 wgpu)
 *   Inspector      → Phase 5
 */

function Eyebrow({ children }: { children: string }) {
  return (
    <div className="border-b-hairline border-bezel-500 px-3 py-2 font-condensed uppercase text-eyebrow text-text-lo">
      {children}
    </div>
  );
}

export function SessionSurfacePlaceholder() {
  return (
    <section
      data-testid="session-surface"
      className="flex min-w-0 flex-1 flex-col overflow-hidden bg-chassis-900"
    >
      <div className="flex h-tabstrip items-center border-b-hairline border-bezel-500 bg-chassis-800 px-3 text-small text-text-lo">
        Phase 4 — tab strip and link strips
      </div>
      <div className="term flex-1 p-3 text-small text-text-mid">
        Phase 6 — wgpu terminal surface. Not an HTML element, not xterm.js.
      </div>
    </section>
  );
}

export function InspectorPlaceholder() {
  return (
    <aside
      data-testid="inspector"
      className="flex w-inspector shrink-0 flex-col overflow-hidden border-l-hairline border-bezel-500 bg-chassis-800"
    >
      <Eyebrow>inspector</Eyebrow>
      <p className="p-3 text-small text-text-lo">Phase 5 — live session metrics.</p>
    </aside>
  );
}
