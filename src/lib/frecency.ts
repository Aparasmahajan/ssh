import { load, save } from "@/lib/persist";

/**
 * Frecency ranking for the command palette.
 *
 * UI_BUILD_GUIDE §0.7: "Recent selections rank first, then fuzzy score.
 * Frecency, not pure fuzzy."
 *
 * Pure fuzzy is wrong for a palette because the thing you want is usually the
 * thing you wanted last time. Pure recency is also wrong, because a host opened
 * fifty times last month should outrank one opened once an hour ago. Frecency is
 * the standard compromise: count, decayed by age.
 */

const KEY = "frecency";
/** Keep the map small; a palette with 10k remembered entries helps nobody. */
const MAX_ENTRIES = 400;
/** A selection loses half its weight after this long. Two weeks matches how
 *  long an ops task tends to stay relevant. */
const HALF_LIFE_MS = 14 * 24 * 60 * 60 * 1000;

interface Entry {
  /** Sum of decayed hits, recomputed on each use. */
  score: number;
  /** When `score` was last brought up to date. */
  at: number;
}

type Store = Record<string, Entry>;

let store: Store = load<Store>(KEY, {});

function decay(entry: Entry, now: number): number {
  const age = Math.max(0, now - entry.at);
  return entry.score * Math.pow(0.5, age / HALF_LIFE_MS);
}

/** Record that `id` was chosen. */
export function recordUse(id: string, now = Date.now()): void {
  const existing = store[id];
  const decayed = existing ? decay(existing, now) : 0;
  store[id] = { score: decayed + 1, at: now };

  if (Object.keys(store).length > MAX_ENTRIES) {
    // Drop the coldest entries rather than letting the map grow without bound.
    const ranked = Object.entries(store)
      .map(([k, v]) => [k, decay(v, now)] as const)
      .sort((a, b) => b[1] - a[1])
      .slice(0, MAX_ENTRIES);
    store = Object.fromEntries(ranked.map(([k]) => [k, store[k] as Entry]));
  }
  save(KEY, store);
}

/**
 * Ranking boost for `id`, in the same units as the fuzzy score.
 *
 * Scaled so a strongly-frecent item outranks a slightly better textual match,
 * but a *much* better match still wins — typing an exact name should always find
 * it, however long ago you last used it.
 */
export function boost(id: string, now = Date.now()): number {
  const e = store[id];
  if (!e) return 0;
  const d = decay(e, now);
  // Logarithmic: the tenth use matters far less than the second.
  return Math.round(24 * Math.log2(1 + d));
}

/** Ids in frecency order — what the palette shows before anything is typed. */
export function recent(limit: number, now = Date.now()): string[] {
  return Object.entries(store)
    .map(([k, v]) => [k, decay(v, now)] as const)
    .sort((a, b) => b[1] - a[1])
    .slice(0, limit)
    .map(([k]) => k);
}

/** Test seam. */
export function _reset(): void {
  store = {};
  save(KEY, store);
}
