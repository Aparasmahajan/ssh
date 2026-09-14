/**
 * Middle truncation.
 *
 * UI_BUILD_GUIDE §0.6: "Truncate middle, not end — `10.20.4.11 · ubuntu…` keeps
 * the useful part." CSS `text-overflow: ellipsis` can only truncate at the end,
 * so this is done in JS against a character budget.
 *
 * A character budget is exact here and only here: the address line is set in
 * IBM Plex Mono, so every glyph has the same advance and `chars = width /
 * advance` is not an approximation. Never reuse this for proportional text.
 */

/** IBM Plex Mono advance width at 11px = 0.6em. */
export const MONO_ADVANCE_11PX = 6.6;

export function charBudget(pxWidth: number, advance = MONO_ADVANCE_11PX): number {
  return Math.max(0, Math.floor(pxWidth / advance));
}

export function middleTruncate(text: string, maxChars: number): string {
  if (maxChars <= 0) return "";
  if (text.length <= maxChars) return text;
  if (maxChars <= 1) return "…";

  // Bias the kept text toward the head: the head of an address or a path is
  // what identifies it. Tail keeps enough to disambiguate siblings.
  const keep = maxChars - 1;
  const head = Math.ceil(keep * 0.6);
  const tail = keep - head;
  return text.slice(0, head) + "…" + (tail > 0 ? text.slice(text.length - tail) : "");
}
