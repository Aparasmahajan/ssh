/** Platform-conditional modifier key.
 *
 * UI_BUILD_GUIDE writes every binding as ⌘X because it was written on a Mac.
 * On Windows and Linux the same bindings are Ctrl. Resolve it once, here, so no
 * component ever hardcodes a platform assumption — including the hint labels,
 * which must match what the key handler actually listens for.
 */
export const IS_APPLE: boolean =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform ?? "");

export const MOD_LABEL = IS_APPLE ? "⌘" : "Ctrl";
export const SHIFT_LABEL = IS_APPLE ? "⇧" : "Shift";
export const ENTER_LABEL = "⏎";

/** True when the event carries the platform's primary modifier and nothing foreign. */
export function hasMod(e: KeyboardEvent): boolean {
  return IS_APPLE ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

/** e.g. keyHint("B") -> "Ctrl B" / "⌘B" */
export function keyHint(...parts: string[]): string {
  return IS_APPLE ? parts.join("") : parts.join("+");
}
