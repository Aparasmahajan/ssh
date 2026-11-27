/**
 * Vendor the woff2 faces that src/styles/fonts.css declares.
 *
 * Offline-first is a product requirement (CLAUDE.md §12, UI_BUILD_GUIDE §0.9):
 * there is NO CDN, so the bytes must sit in src/styles/fonts/ under the exact
 * filenames fonts.css asks for. @fontsource is the source because it is an
 * ordinary npm dependency — no network fetch at build or run time.
 *
 * Each face is vendored as TWO slices, `latin` and `latin-ext`, declared in
 * fonts.css with fontsource's own unicode-ranges. Fontsource's `latin-ext` is
 * NOT a superset of `latin`: it has no a-z. This script used to pick it as the
 * "wider" subset, and until 2026-11-26 the whole UI rendered in the system
 * fallback face with no error anywhere.
 *
 * JetBrains Mono is not vendored here. fonts.css takes it from
 * crates/render/fonts/, the same files the GPU terminal renderer embeds, so
 * both paths draw the same face and both have box drawing (no fontsource
 * slice covers U+2500).
 */
import { copyFile, mkdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT = path.join(ROOT, "src", "styles", "fonts");

const SLICES = ["latin", "latin-ext"];

/** dest basename (fonts.css adds `-<slice>.woff2`) -> @fontsource package + weight */
const FACES = [
  ["ibm-plex-sans-400", "@fontsource/ibm-plex-sans", 400],
  ["ibm-plex-sans-500", "@fontsource/ibm-plex-sans", 500],
  ["ibm-plex-sans-condensed-600", "@fontsource/ibm-plex-sans-condensed", 600],
  ["ibm-plex-mono-400", "@fontsource/ibm-plex-mono", 400],
  ["ibm-plex-mono-500", "@fontsource/ibm-plex-mono", 500],
];

const problems = [];
await mkdir(OUT, { recursive: true });

for (const [dest, pkg, weight] of FACES) {
  const base = pkg.split("/")[1];
  for (const slice of SLICES) {
    const src = path.join(ROOT, "node_modules", ...pkg.split("/"), "files", `${base}-${slice}-${weight}-normal.woff2`);
    if (!existsSync(src)) {
      problems.push(`  ✗ ${dest}-${slice}.woff2 — ${path.relative(ROOT, src)} missing`);
      continue;
    }
    await copyFile(src, path.join(OUT, `${dest}-${slice}.woff2`));
    console.log(`  ✓ ${dest}-${slice}.woff2`);
  }
}

if (problems.length) {
  console.log("\nvendor-fonts: attention needed\n" + problems.join("\n"));
  // Non-fatal: `npm install` must not fail over this, but it must be loud.
}
