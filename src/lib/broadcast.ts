/**
 * Which Rust session id each pane is driving.
 *
 * Broadcast writes one keystroke to every pane in a tab, and the session ids
 * live inside each `TerminalSurface`. Lifting them into React state would make
 * every surface re-render whenever any other surface opened, so panes register
 * here instead.
 *
 * A plain module-level map, deliberately not a store: nothing renders from it.
 */

const paneSessions = new Map<string, number>();

export function registerPane(paneId: string, sessionId: number): void {
  paneSessions.set(paneId, sessionId);
}

export function unregisterPane(paneId: string): void {
  paneSessions.delete(paneId);
}

/** Session ids for the given panes, skipping any that have not opened yet. */
export function sessionsFor(paneIds: readonly string[]): number[] {
  return paneIds
    .map((p) => paneSessions.get(p))
    .filter((id): id is number => id !== undefined);
}
