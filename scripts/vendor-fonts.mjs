/**
 * Vendor the woff2 faces that src/styles/fonts.css declares.
 *
 * Offline-first is a product requirement (CLAUDE.md §12, UI_BUILD_GUIDE §0.9):
 * there is NO CDN, so the bytes must sit in src/styles/fonts/ under the exact
 * filenames fonts.css asks for. @fontsource is the source because it is an
 * ordinary npm dependency — no network fetch at build or run time.
 *
 * Subset preference is widest-first. The mono faces MUST retain U+2500–259F or
 * the terminal renders `systemd` trees as tofu, so this script reports whether
 * the subset it picked actually covers box drawing.
 */
import { readdir, readFile, copyFile, mkdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT = path.join(ROOT, "src", "styles", "fonts");

/** Widest coverage first. `all` is unsubsetted and is what we want. */
const SUBSET_PREFERENCE = ["all", "latin-ext", "latin"];

/** dest filename (as declared in fonts.css) -> @fontsource package + weight */
const FACES = [
  ["ibm-plex-sans-400.woff2", "@fontsource/ibm-plex-sans", 400],
  ["ibm-plex-sans-500.woff2", "@fontsource/ibm-plex-sans", 500],
  ["ibm-plex-sans-condensed-600.woff2", "@fontsource/ibm-plex-sans-condensed", 600],
  ["ibm-plex-mono-400.woff2", "@fontsource/ibm-plex-mono", 400],
  ["ibm-plex-mono-500.woff2", "@fontsource/ibm-plex-mono", 500],
  ["jetbrains-mono-400.woff2", "@fontsource/jetbrains-mono", 400],
  ["jetbrains-mono-700.woff2", "@fontsource/jetbrains-mono", 700],
];

/** Does this package's stylesheet for `subset` declare coverage of U+2500? */
async function coversBoxDrawing(pkgDir, subset) {
  const css = path.join(pkgDir, `${subset}.css`);
  if (!existsSync(css)) return null; // no per-subset css shipped; unknown
  const text = await readFile(css, "utf8");
  const ranges = [...text.matchAll(/U\+([0-9A-Fa-f]+)(?:-([0-9A-Fa-f]+))?/g)];
  return ranges.some(([, lo, hi]) => {
    const a = parseInt(lo, 16);
    const b = hi ? parseInt(hi, 16) : a;
    return a <= 0x2500 && 0x2500 <= b;
  });
}

async function pick(pkg, weight) {
  const pkgDir = path.join(ROOT, "node_modules", ...pkg.split("/"));
  const filesDir = path.join(pkgDir, "files");
  if (!existsSync(filesDir)) return { error: `${pkg} not installed` };

  const files = await readdir(filesDir);
  for (const subset of SUBSET_PREFERENCE) {
    const name = files.find((f) => f.endsWith(`-${subset}-${weight}-normal.woff2`));
    if (name) {
      return { src: path.join(filesDir, name), subset, box: await coversBoxDrawing(pkgDir, subset) };
    }
  }
  return { error: `no woff2 for ${pkg} @ ${weight} (looked for ${SUBSET_PREFERENCE.join(", ")})` };
}

const problems = [];
await mkdir(OUT, { recursive: true });

for (const [dest, pkg, weight] of FACES) {
  const found = await pick(pkg, weight);
  if (found.error) {
    problems.push(`  ✗ ${dest} — ${found.error}`);
    continue;
  }
  await copyFile(found.src, path.join(OUT, dest));
  const isMono = dest.includes("mono");
  const boxNote = !isMono ? "" : found.box === false ? "  ⚠ NO BOX DRAWING (U+2500)" : found.box === true ? "  ✓ box drawing" : "  ? box drawing unknown";
  console.log(`  ✓ ${dest}  [${found.subset}]${boxNote}`);
  if (isMono && found.box === false) {
    problems.push(
      `  ⚠ ${dest} subset "${found.subset}" omits U+2500–259F. Terminal box drawing will render as tofu. ` +
        `Fix: subset the full TTF with the pyftsubset command in src/styles/fonts.css.`,
    );
  }
}

if (problems.length) {
  console.log("\nvendor-fonts: attention needed\n" + problems.join("\n"));
  // Non-fatal: `npm install` must not fail over this, but it must be loud.
}
