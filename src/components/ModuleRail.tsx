import { MODULES, useLayout, type ModuleId } from "@/state/layout";
import { Tooltip } from "./Tooltip";

/**
 * UI_BUILD_GUIDE §0.5 zone 1 — 48px, icon-only, vertical.
 * Active module gets a 3px amber left edge. No labels; tooltips after 600ms.
 * Screen-reader labels on every icon-only control (§0.8).
 *
 * Glyphs are text, not an icon package: an icon library is 20–60 KB of JS
 * against a 250ms cold-start budget (CLAUDE.md §4, §14.5) to draw seven marks.
 */
export function ModuleRail() {
  const activeModule = useLayout((s) => s.activeModule);
  const setActiveModule = useLayout((s) => s.setActiveModule);

  return (
    <nav
      aria-label="Modules"
      data-testid="module-rail"
      className="flex w-rail flex-col items-stretch border-r-hairline border-bezel-500 bg-chassis-800 py-2"
    >
      {MODULES.map((m) => (
        <RailButton
          key={m.id}
          id={m.id}
          glyph={m.glyph}
          label={m.label}
          active={activeModule === m.id}
          onSelect={setActiveModule}
        />
      ))}
    </nav>
  );
}

function RailButton({
  id,
  glyph,
  label,
  active,
  onSelect,
}: {
  id: ModuleId;
  glyph: string;
  label: string;
  active: boolean;
  onSelect: (id: ModuleId) => void;
}) {
  return (
    <Tooltip label={label}>
      <button
        type="button"
        aria-label={label}
        aria-current={active ? "page" : undefined}
        onClick={() => onSelect(id)}
        className={`relative flex h-10 w-rail items-center justify-center text-large
                    transition-colors duration-hover
                    ${active ? "bg-chassis-700 text-amber-500" : "text-text-lo hover:text-text-mid"}`}
      >
        {/* 3px amber left edge — the only active indicator. */}
        <span
          aria-hidden
          className={`absolute inset-y-0 left-0 w-[3px] ${active ? "bg-amber-500" : "bg-transparent"}`}
        />
        <span aria-hidden>{glyph}</span>
      </button>
    </Tooltip>
  );
}
