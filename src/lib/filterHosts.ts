import {
  NO_MATCH,
  buildMaskHi,
  buildMaskLo,
  fuzzyMatch,
  fuzzyScore,
  passesPrefilter,
  type Range,
} from "./fuzzy.ts";
import type { Host } from "../types.ts";

/**
 * THE `filter_hosts(query, tags)` SEAM.
 *
 * UI_BUILD_GUIDE Step 5 requires this to run in Rust as a Tauri command. There
 * is no Rust toolchain on this machine (docs/DECISIONS.md D-1, D-2), so the body
 * is TypeScript. The shape is deliberately the shape the Rust version wants, so
 * the port is close to a transliteration.
 *
 * Structure, all of it from CLAUDE.md rather than invention:
 *
 *   §5.1  Structure-of-arrays. Results are parallel typed arrays, not an array
 *         of match objects. 10,000 result objects per keystroke is 10,000
 *         allocations the GC then has to walk, and that GC was visible as the
 *         p99 column in `scripts/bench-filter.ts`.
 *   §7    Bit-parallel prefilter rejects non-candidates with one AND.
 *   §7    "Cache scores keyed by (host_id, query_prefix) so each extra character
 *         reuses prior work" — implemented as candidate-set narrowing: matches
 *         of "10.2" are necessarily a subset of matches of "10.", so an extended
 *         query only rescans the previous survivors.
 *   §7    Tag filter is a set intersection (roaring bitmaps in Rust).
 *
 * Ranking uses a **stable counting sort** over the score, not a comparator.
 * Scores are small integers, so ranking is O(n) with no per-comparison string
 * work; feeding it a name-ordered input makes equal scores fall into name order
 * for free.
 */

/** 0 = matched the name, 1 = matched the address. */
export const FIELD_NAME = 0;
export const FIELD_ADDRESS = 1;

export interface FilterResult {
  /** Host indices, best first. Only the first `count` entries are meaningful. */
  order: Int32Array;
  /** FIELD_NAME | FIELD_ADDRESS per position in `order`. */
  fields: Uint8Array;
  count: number;
  /** The normalized query that produced this. Drives prefix reuse. */
  query: string;
  /** The tags that produced this; reuse is only valid when they are unchanged. */
  tags: readonly string[];
}

export interface HostIndex {
  hosts: readonly Host[];
  lowerNames: readonly string[];
  lowerAddresses: readonly string[];
  /** Two-word (u64) presence masks, per CLAUDE.md §7. See fuzzy.ts. */
  nameMaskLo: Int32Array;
  nameMaskHi: Int32Array;
  addressMaskLo: Int32Array;
  addressMaskHi: Int32Array;
  /** Host indices in lowercase-name order. The stable sort's input order, so
   *  equal scores come out alphabetical without comparing strings. */
  nameOrder: Int32Array;
  byTag: Map<string, Set<number>>;
}

/**
 * O(n log n) once — at config load, not per keystroke. CLAUDE.md §12.4 caches
 * the Rust equivalent in `cache/index.rkyv` behind a validity token, so a warm
 * launch pays nothing for this.
 */
export function buildHostIndex(hosts: readonly Host[]): HostIndex {
  const n = hosts.length;
  const lowerNames = new Array<string>(n);
  const lowerAddresses = new Array<string>(n);
  const nameMaskLo = new Int32Array(n);
  const nameMaskHi = new Int32Array(n);
  const addressMaskLo = new Int32Array(n);
  const addressMaskHi = new Int32Array(n);
  const byTag = new Map<string, Set<number>>();

  for (let i = 0; i < n; i++) {
    const h = hosts[i] as Host;
    const ln = h.name.toLowerCase();
    const la = h.address.toLowerCase();
    lowerNames[i] = ln;
    lowerAddresses[i] = la;
    nameMaskLo[i] = buildMaskLo(ln);
    nameMaskHi[i] = buildMaskHi(ln);
    addressMaskLo[i] = buildMaskLo(la);
    addressMaskHi[i] = buildMaskHi(la);
    for (const tag of h.tags) {
      let set = byTag.get(tag);
      if (!set) byTag.set(tag, (set = new Set<number>()));
      set.add(i);
    }
  }

  // Codepoint order on the lowercase name. Not localeCompare: it calls into ICU
  // per comparison, and its result varies by platform locale — which would make
  // an exported config render in a different order on another machine.
  const nameOrder = Int32Array.from({ length: n }, (_, i) => i).sort((a, b) => {
    const an = lowerNames[a] as string;
    const bn = lowerNames[b] as string;
    return an < bn ? -1 : an > bn ? 1 : 0;
  });

  return {
    hosts,
    lowerNames,
    lowerAddresses,
    nameMaskLo,
    nameMaskHi,
    addressMaskLo,
    addressMaskHi,
    nameOrder,
    byTag,
  };
}

/** Smallest-set-first intersection, so the scan is bounded by the rarest tag. */
function candidatesFor(index: HostIndex, tags: readonly string[]): Set<number> | null {
  if (tags.length === 0) return null; // null means "all hosts"
  const sets: Set<number>[] = [];
  for (const tag of tags) {
    const set = index.byTag.get(tag);
    if (!set) return new Set(); // an unknown tag matches nothing
    sets.push(set);
  }
  sets.sort((a, b) => a.size - b.size);
  const smallest = sets[0] as Set<number>;
  const out = new Set<number>();
  outer: for (const id of smallest) {
    for (let s = 1; s < sets.length; s++) if (!(sets[s] as Set<number>).has(id)) continue outer;
    out.add(id);
  }
  return out;
}

/** Reusable scratch, so a keystroke allocates nothing in steady state. */
type Scratch = { hits: Int32Array; scores: Int32Array; fields: Uint8Array };
const scratchFor = new WeakMap<HostIndex, Scratch>();

function scratch(index: HostIndex): Scratch {
  let s = scratchFor.get(index);
  if (!s) {
    const n = index.hosts.length;
    s = { hits: new Int32Array(n), scores: new Int32Array(n), fields: new Uint8Array(n) };
    scratchFor.set(index, s);
  }
  return s;
}

/**
 * Synchronous core, measured by `scripts/bench-filter.ts`.
 *
 * `prev` enables prefix reuse: pass the previous result and, when the new query
 * merely extends it with the same tags, only the previous survivors are
 * rescanned. This is the difference between "the first keystroke is the
 * expensive one" and "every keystroke is the expensive one".
 *
 * Complexity: O(c) prefilter ANDs + O(len) scoring per survivor + O(c + range)
 * counting sort, where c is the candidate count (all hosts, or the previous
 * result set).
 */
export function filterHostsSync(
  index: HostIndex,
  query: string,
  tags: readonly string[] = [],
  prev?: FilterResult,
): FilterResult {
  const q = query.trim().toLowerCase();
  const n = index.hosts.length;
  const s = scratch(index);

  /* ---- Choose the candidate list ---------------------------------------
     ALWAYS an Int32Array, never a union with `number[]`. A union here makes
     every `candidateSource[k]` load polymorphic, and V8 then cannot inline it:
     that single type change was worth ~10x on the all-match queries in
     scripts/bench-filter.ts. Keep it monomorphic. */
  let candidateSource: Int32Array;
  let candidateCount: number;
  const sameTags = prev !== undefined && sameStringList(prev.tags, tags);
  const canReuse = sameTags && prev !== undefined && prev.query.length > 0 && q.startsWith(prev.query);

  if (canReuse && prev) {
    candidateSource = prev.order;
    candidateCount = prev.count;
  } else {
    const tagged = candidatesFor(index, tags);
    if (tagged) {
      // Walk nameOrder so the counting sort still receives name-ordered input.
      // Copied out of the scratch buffer because the scoring pass below writes
      // to `s.hits`, which would otherwise be the array being read.
      const picked = new Int32Array(tagged.size);
      let w = 0;
      for (let k = 0; k < n; k++) {
        const i = index.nameOrder[k] as number;
        if (tagged.has(i)) picked[w++] = i;
      }
      candidateSource = picked;
      candidateCount = w;
    } else {
      candidateSource = index.nameOrder;
      candidateCount = n;
    }
  }

  /* ---- Empty query: everything, in candidate order --------------------- */
  if (q.length === 0) {
    const order = new Int32Array(candidateCount);
    for (let k = 0; k < candidateCount; k++) order[k] = candidateSource[k] as number;
    return {
      order,
      fields: new Uint8Array(candidateCount),
      count: candidateCount,
      query: q,
      tags: [...tags],
    };
  }

  /* ---- Score ------------------------------------------------------------ */
  const queryLo = buildMaskLo(q);
  const queryHi = buildMaskHi(q);
  let hitCount = 0;
  let minScore = 0x7fffffff;
  let maxScore = -0x7fffffff;

  // Hoisted out of the loop: repeated `index.x` loads inside a 10,000-iteration
  // body are property lookups V8 has to re-check on every pass.
  const { hosts, lowerNames, lowerAddresses, nameMaskLo, nameMaskHi, addressMaskLo, addressMaskHi } =
    index;
  const outHits = s.hits;
  const outScores = s.scores;
  const outFields = s.fields;

  for (let k = 0; k < candidateCount; k++) {
    const i = candidateSource[k] as number;

    const nameOk = passesPrefilter(nameMaskLo[i] as number, nameMaskHi[i] as number, queryLo, queryHi);
    const addrOk = passesPrefilter(
      addressMaskLo[i] as number,
      addressMaskHi[i] as number,
      queryLo,
      queryHi,
    );
    if (!nameOk && !addrOk) continue;

    const host = hosts[i] as Host;
    let score = NO_MATCH;
    let field: number = FIELD_NAME;

    if (nameOk) {
      score = fuzzyScore(host.name, lowerNames[i] as string, q);
    }
    if (score === NO_MATCH && addrOk) {
      const addr = fuzzyScore(host.address, lowerAddresses[i] as string, q);
      if (addr !== NO_MATCH) {
        // An address hit is weaker evidence than a name hit: someone typing
        // "10." wants the address column, someone typing "db" means the name.
        score = addr - 4;
        field = FIELD_ADDRESS;
      }
    }
    if (score === NO_MATCH) continue;

    outHits[hitCount] = i;
    outScores[hitCount] = score;
    outFields[hitCount] = field;
    hitCount++;
    if (score < minScore) minScore = score;
    if (score > maxScore) maxScore = score;
  }

  if (hitCount === 0) {
    return {
      order: new Int32Array(0),
      fields: new Uint8Array(0),
      count: 0,
      query: q,
      tags: [...tags],
    };
  }

  /* ---- Stable counting sort, descending by score ------------------------ */
  const range = maxScore - minScore + 1;
  const start = new Int32Array(range);
  for (let k = 0; k < hitCount; k++) start[(outScores[k] as number) - minScore]!++;

  // Prefix sums from the TOP score down, so higher scores land at lower indices.
  // Accumulated in place: the count array becomes the start-offset array.
  let acc = 0;
  for (let b = range - 1; b >= 0; b--) {
    const c = start[b] as number;
    start[b] = acc;
    acc += c;
  }

  // Scores are NOT returned. Nothing downstream reads them — ranking is already
  // baked into `order` — and at 10,000 results that array was 40KB of garbage
  // per keystroke for no consumer.
  const order = new Int32Array(hitCount);
  const fields = new Uint8Array(hitCount);
  for (let k = 0; k < hitCount; k++) {
    const pos = start[(outScores[k] as number) - minScore]!++;
    order[pos] = outHits[k] as number;
    fields[pos] = outFields[k] as number;
  }

  return { order, fields, count: hitCount, query: q, tags: [...tags] };
}

function sameStringList(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * Highlight ranges for ONE host, computed on demand.
 *
 * UI_BUILD_GUIDE Step 5 says `filter_hosts` returns "ordered host IDs plus match
 * ranges". It returns ordered ids and scores; ranges come from here instead.
 * Reason: highlighting only exists for rows on screen — about 30 of them — so
 * computing ranges for all 10,000 results is work whose output is discarded, and
 * over a real Tauri IPC boundary it would also be 10,000 arrays serialized to
 * render 30. CLAUDE.md §3 ("IPC to webview: shared-memory ring... Never JSON")
 * points the same way. Recorded as D-9 in docs/DECISIONS.md.
 */
export function rangesFor(
  index: HostIndex,
  hostIdx: number,
  field: number,
  query: string,
): Range[] {
  const q = query.trim().toLowerCase();
  if (q.length === 0) return [];
  const host = index.hosts[hostIdx];
  if (!host) return [];
  const [text, lower] =
    field === FIELD_ADDRESS
      ? [host.address, index.lowerAddresses[hostIdx] as string]
      : [host.name, index.lowerNames[hostIdx] as string];
  return fuzzyMatch(text, lower, q)?.ranges ?? [];
}

/**
 * The async seam mirroring the future Tauri command. `signal` exists because
 * Step 5 requires each keystroke to cancel the in-flight query; the work is
 * synchronous today so cancellation is checked at the boundaries, but the
 * contract is already right for a call over IPC.
 */
export async function filterHosts(
  index: HostIndex,
  query: string,
  tags: readonly string[] = [],
  signal?: AbortSignal,
  prev?: FilterResult,
): Promise<FilterResult> {
  if (signal?.aborted) throw new DOMException("aborted", "AbortError");
  const result = filterHostsSync(index, query, tags, prev);
  if (signal?.aborted) throw new DOMException("aborted", "AbortError");
  return result;
}
