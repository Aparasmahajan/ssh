/**
 * Fuzzy matching for the host tree and command palette.
 *
 * Structure follows CLAUDE.md §7: a **bit-parallel prefilter** that rejects
 * non-candidates with one AND, then scoring on the survivors only. At 10,000
 * hosts the prefilter is what makes the 3ms budget reachable — scoring every
 * host would not be.
 *
 * HONEST SCOPE: the scorer here is fzf's *v1* algorithm (greedy forward pass,
 * backward tighten, then a single scoring sweep with boundary/camel/consecutive
 * bonuses). CLAUDE.md §7 asks for fzf **v2**, whose Smith-Waterman DP finds the
 * optimal alignment rather than a good one. v1 is O(n) per candidate instead of
 * O(n·m), and differs from v2 only in ranking a small number of ambiguous
 * candidates. The DP belongs in the Rust port (docs/DECISIONS.md D-2) where the
 * per-candidate cost is affordable. Do not "upgrade" this in TS.
 *
 * This module has no imports on purpose: it is benchmarked directly by
 * `scripts/bench-filter.ts` under bare node.
 */

/** [start, end) over the *original* string, for highlighting. */
export type Range = readonly [start: number, end: number];

export interface FuzzyMatch {
  score: number;
  ranges: Range[];
}

/* ---- Bit-parallel prefilter ----------------------------------------------
   CLAUDE.md §7 specifies a **u64** per pattern. JS bitwise operators are 32-bit,
   so the 64-bit mask is carried as two `number` words rather than one.

   This matters more than it looks. An earlier 31-bit version had to collapse all
   ten digits into a single bucket, which made the query "1" pass every host whose
   name contained ANY digit — and since every hostname ends in an ordinal, that
   was all of them. The prefilter rejected nothing and 10,000 candidates went to
   the scorer twice each (name, then address). Measured cost of that one lost bit:
   5.2ms of a 5ms budget. Widening to two words is what made digits separable.

   LO word: 'a'–'z'          → bits 0–25
   HI word: '0'–'9'          → bits 0–9
            '.' '-' '_' '/' ':' → bits 10–14
            anything else     → bit 15
-------------------------------------------------------------------------- */

const A = 97; // 'a'
const Z = 122;
const ZERO = 48;
const NINE = 57;

const HI_DOT = 10;
const HI_DASH = 11;
const HI_UNDERSCORE = 12;
const HI_SLASH = 13;
const HI_COLON = 14;
const HI_OTHER = 15;

/** Presence bitmap, low word: the ASCII lowercase letters. */
export function buildMaskLo(lower: string): number {
  let mask = 0;
  for (let i = 0; i < lower.length; i++) {
    const c = lower.charCodeAt(i);
    if (c >= A && c <= Z) mask |= 1 << (c - A);
  }
  return mask;
}

/** Presence bitmap, high word: digits individually, then separators. */
export function buildMaskHi(lower: string): number {
  let mask = 0;
  for (let i = 0; i < lower.length; i++) {
    const c = lower.charCodeAt(i);
    if (c >= A && c <= Z) continue;
    if (c >= ZERO && c <= NINE) mask |= 1 << (c - ZERO);
    else if (c === 46) mask |= 1 << HI_DOT;
    else if (c === 45) mask |= 1 << HI_DASH;
    else if (c === 95) mask |= 1 << HI_UNDERSCORE;
    else if (c === 47) mask |= 1 << HI_SLASH;
    else if (c === 58) mask |= 1 << HI_COLON;
    else mask |= 1 << HI_OTHER;
  }
  return mask;
}

/**
 * True when every character in the query is present in the candidate.
 *
 * False is a definite non-match. True still needs scoring: the mask ignores
 * order and multiplicity, deliberately — that is exactly what makes it two ANDs
 * instead of a scan.
 */
export function passesPrefilter(
  hostLo: number,
  hostHi: number,
  queryLo: number,
  queryHi: number,
): boolean {
  return (queryLo & ~hostLo) === 0 && (queryHi & ~hostHi) === 0;
}

/* ---- Scoring ------------------------------------------------------------- */

const SCORE_MATCH = 16;
const BONUS_BOUNDARY = 8;
const BONUS_CAMEL = 7;
const BONUS_NON_WORD = 8;
const BONUS_CONSECUTIVE = 8;
/** A match on the first pattern char is worth double its bonus: users type
 *  prefixes far more often than infixes, so "lin" should rank linux-01 above
 *  berlin-db. */
const BONUS_FIRST_CHAR_MULTIPLIER = 2;
const SCORE_GAP_START = -3;
const SCORE_GAP_EXTENSION = -1;

const CLASS_WHITE = 0;
const CLASS_NON_WORD = 1;
const CLASS_DIGIT = 2;
const CLASS_LOWER = 3;
const CLASS_UPPER = 4;

function classOf(code: number): number {
  if (code === 32 || code === 9) return CLASS_WHITE;
  if (code >= ZERO && code <= NINE) return CLASS_DIGIT;
  if (code >= A && code <= Z) return CLASS_LOWER;
  if (code >= 65 && code <= 90) return CLASS_UPPER;
  return CLASS_NON_WORD;
}

function bonusFor(prev: number, cur: number): number {
  if (cur !== CLASS_NON_WORD && cur !== CLASS_WHITE && (prev === CLASS_NON_WORD || prev === CLASS_WHITE)) {
    return BONUS_BOUNDARY;
  }
  if ((prev === CLASS_LOWER && cur === CLASS_UPPER) || (prev !== CLASS_DIGIT && cur === CLASS_DIGIT)) {
    return BONUS_CAMEL;
  }
  if (cur === CLASS_NON_WORD || cur === CLASS_WHITE) return BONUS_NON_WORD;
  return 0;
}

/** Returned by `fuzzyScore` when the pattern is not present at all. */
export const NO_MATCH = -0x7fffffff;

/**
 * Score only — no allocation, no ranges. This is the per-keystroke path.
 *
 * Ranges are a *paint* concern: only the ~30 rows actually on screen need them,
 * so materializing them for 10,000 candidates is pure waste, and it was the
 * dominant GC pressure in the p99 column of `scripts/bench-filter.ts` before
 * this split existed. Call `fuzzyMatch` for the visible rows instead.
 */
export function fuzzyScore(text: string, lower: string, lowerPattern: string): number {
  const m = run(text, lower, lowerPattern, null);
  return m === null ? NO_MATCH : m;
}

/**
 * Score `lower` against `lowerPattern`, WITH highlight ranges. Returns null on
 * no match. `text` is the original-case string; case drives the camel bonus only.
 *
 * O(len(text)) — two linear scans plus one scoring scan over a tightened window.
 */
export function fuzzyMatch(text: string, lower: string, lowerPattern: string): FuzzyMatch | null {
  if (lowerPattern.length === 0) return { score: 0, ranges: [] };
  const positions: number[] = [];
  const score = run(text, lower, lowerPattern, positions);
  return score === null ? null : { score, ranges: toRanges(positions) };
}

/**
 * The shared scorer. `positions` collects match indices when non-null; passing
 * null skips that work entirely. Returns null on no match.
 */
function run(
  text: string,
  lower: string,
  lowerPattern: string,
  positions: number[] | null,
): number | null {
  const plen = lowerPattern.length;
  if (plen === 0) return 0;

  const tlen = lower.length;
  if (plen > tlen) return null;

  // Pass 1: greedy forward — cheapest possible proof that a match exists.
  let pidx = 0;
  let sidx = -1;
  let eidx = -1;
  for (let i = 0; i < tlen; i++) {
    if (lower.charCodeAt(i) === lowerPattern.charCodeAt(pidx)) {
      if (sidx < 0) sidx = i;
      if (++pidx === plen) {
        eidx = i + 1;
        break;
      }
    }
  }
  if (eidx < 0) return null;

  // Pass 2: greedy backward — tighten the window so scoring sees the densest
  // region. Without this, "l1" against "linux-01" would score across the whole
  // string and lose to a worse but shorter candidate.
  pidx = plen - 1;
  for (let i = eidx - 1; i >= sidx; i--) {
    if (lower.charCodeAt(i) === lowerPattern.charCodeAt(pidx)) {
      if (--pidx < 0) {
        sidx = i;
        break;
      }
    }
  }

  // Pass 3: score the window and collect positions.
  let score = 0;
  let inGap = false;
  let consecutive = 0;
  let firstBonus = 0;
  let prevClass = sidx > 0 ? classOf(text.charCodeAt(sidx - 1)) : CLASS_WHITE;
  pidx = 0;

  for (let i = sidx; i < eidx; i++) {
    const cur = classOf(text.charCodeAt(i));
    if (lower.charCodeAt(i) === lowerPattern.charCodeAt(pidx)) {
      if (positions !== null) positions.push(i);
      score += SCORE_MATCH;
      let bonus = bonusFor(prevClass, cur);
      if (consecutive === 0) {
        firstBonus = bonus;
      } else {
        // A run keeps at least the run's opening bonus: consecutive characters
        // are strong evidence regardless of where the run started.
        bonus = Math.max(bonus, Math.max(firstBonus, BONUS_CONSECUTIVE));
      }
      score += pidx === 0 ? bonus * BONUS_FIRST_CHAR_MULTIPLIER : bonus;
      inGap = false;
      consecutive++;
      pidx++;
      if (pidx === plen) {
        prevClass = cur;
        break;
      }
    } else {
      score += inGap ? SCORE_GAP_EXTENSION : SCORE_GAP_START;
      inGap = true;
      consecutive = 0;
      firstBonus = 0;
    }
    prevClass = cur;
  }

  return score;
}

/** Collapse sorted indices into contiguous [start, end) spans for highlighting. */
function toRanges(positions: number[]): Range[] {
  const ranges: Range[] = [];
  let i = 0;
  while (i < positions.length) {
    const start = positions[i] as number;
    let end = start + 1;
    while (i + 1 < positions.length && positions[i + 1] === end) {
      end++;
      i++;
    }
    ranges.push([start, end]);
    i++;
  }
  return ranges;
}
