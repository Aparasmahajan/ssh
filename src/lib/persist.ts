/**
 * Persistence adapter for UI chrome state (panel widths, collapse flags).
 *
 * CLAUDE.md §12.1 is explicit that this class of state belongs in the OS state
 * dir as `machine.toml` — "DO NOT COPY. All rebuildable." — not in the portable
 * config. Until the Tauri store exists this is localStorage; the seam is here
 * so swapping the backend touches one file, not every component.
 *
 * Writes are debounced: a drag emits a value per pointermove, and hammering the
 * store from a drag handler is exactly the kind of UI-thread work CLAUDE.md
 * §13.1 rule 1 forbids.
 */
const PREFIX = "opspilot:machine:";
const DEBOUNCE_MS = 250;

const pending = new Map<string, unknown>();
let timer: number | undefined;

function flush() {
  timer = undefined;
  for (const [key, value] of pending) {
    try {
      localStorage.setItem(PREFIX + key, JSON.stringify(value));
    } catch {
      // A full or unavailable store must never break the UI. Layout is
      // rebuildable by definition; losing it costs the user one drag.
    }
  }
  pending.clear();
}

export function load<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    return raw === null ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}

export function save(key: string, value: unknown): void {
  pending.set(key, value);
  if (timer === undefined) timer = window.setTimeout(flush, DEBOUNCE_MS);
}
