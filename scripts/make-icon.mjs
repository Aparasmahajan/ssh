/**
 * Generate the OpsPilot icon source PNG.
 *
 * `tauri-build` needs `src-tauri/icons/icon.ico` on Windows to emit the resource
 * file — without it even `cargo check` fails. Rather than commit a binary blob
 * nobody can edit, the source art is generated here and `npx tauri icon` fans it
 * out into every required size and format.
 *
 * Colours are READ FROM tokens.css, not written here. tokens.css says "no hex
 * literal may appear in any .tsx/.ts file... If a value is missing here, add it
 * here — do not inline it", and an icon that drifts from the palette is exactly
 * the erosion that rule exists to prevent.
 *
 * The mark is the product thesis at icon scale (UI_BUILD_GUIDE §0.1): a faceplate
 * with an amber edge bar and port chips. No gradients, no soft shadows — the same
 * flat-fill constraint as the UI, which is also what survives being scaled to 16px.
 *
 * Run:  node scripts/make-icon.mjs
 */
import { deflateSync } from "node:zlib";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SIZE = 1024;

/* ---- Palette, straight out of tokens.css -------------------------------- */

const tokens = readFileSync(path.join(ROOT, "src", "styles", "tokens.css"), "utf8");

function token(name) {
  const m = tokens.match(new RegExp(`--${name}\\s*:\\s*(#[0-9A-Fa-f]{6})`));
  if (!m) throw new Error(`token --${name} not found in tokens.css`);
  const v = parseInt(m[1].slice(1), 16);
  return [(v >> 16) & 255, (v >> 8) & 255, v & 255, 255];
}

const CHASSIS = token("chassis-700");
const BEZEL = token("bezel-500");
const AMBER = token("amber-500");
const CYAN = token("cyan-400");

/* ---- Tiny RGBA canvas ---------------------------------------------------- */

const px = new Uint8Array(SIZE * SIZE * 4); // transparent

function blend(x, y, [r, g, b, a]) {
  if (x < 0 || y < 0 || x >= SIZE || y >= SIZE) return;
  const i = (y * SIZE + x) * 4;
  if (a === 255) {
    px[i] = r;
    px[i + 1] = g;
    px[i + 2] = b;
    px[i + 3] = 255;
    return;
  }
  const t = a / 255;
  px[i] = Math.round(px[i] * (1 - t) + r * t);
  px[i + 1] = Math.round(px[i + 1] * (1 - t) + g * t);
  px[i + 2] = Math.round(px[i + 2] * (1 - t) + b * t);
  px[i + 3] = Math.max(px[i + 3], a);
}

/**
 * Rounded rectangle with 4x supersampled coverage on the edges. Aliased corners
 * are extremely visible once the icon is scaled to 32px and 16px, and the whole
 * point of generating this is that it survives downscaling.
 */
function roundRect(x0, y0, w, h, radius, colour) {
  const S = 4;
  for (let y = Math.floor(y0); y < Math.ceil(y0 + h); y++) {
    for (let x = Math.floor(x0); x < Math.ceil(x0 + w); x++) {
      let hits = 0;
      for (let sy = 0; sy < S; sy++) {
        for (let sx = 0; sx < S; sx++) {
          const fx = x + (sx + 0.5) / S;
          const fy = y + (sy + 0.5) / S;
          if (fx < x0 || fy < y0 || fx > x0 + w || fy > y0 + h) continue;
          // distance into the nearest corner's circle
          const cx = Math.min(Math.max(fx, x0 + radius), x0 + w - radius);
          const cy = Math.min(Math.max(fy, y0 + radius), y0 + h - radius);
          const dx = fx - cx;
          const dy = fy - cy;
          if (dx * dx + dy * dy <= radius * radius) hits++;
        }
      }
      if (hits === 0) continue;
      blend(x, y, [colour[0], colour[1], colour[2], Math.round((hits / (S * S)) * 255)]);
    }
  }
}

/* ---- The mark ------------------------------------------------------------ */

// Faceplate. 180 radius reads as a rounded square at 1024 and stays square-ish
// at 16px, where an over-rounded icon turns into an indistinct blob.
roundRect(0, 0, SIZE, SIZE, 180, CHASSIS);

// The environment tie bar — 3px in the UI, the one element present on every
// host row, and the most recognisable thing in the product at any scale.
roundRect(150, 232, 78, 560, 39, AMBER);

// Port chips. Top one amber = active connection; the rest bezel = available.
const CHIP_X = 300;
const CHIP_W = 574;
const CHIP_H = 118;
const CHIP_R = 30;
const ROWS = [232, 453, 674];
ROWS.forEach((y, i) => roundRect(CHIP_X, y, CHIP_W, CHIP_H, CHIP_R, i === 0 ? AMBER : BEZEL));

// One cyan link LED. Cyan is link-state only (§0.2) and this is link state.
roundRect(CHIP_X + CHIP_W - 200, ROWS[1] + 29, 60, 60, 30, CYAN);

/* ---- PNG encode ---------------------------------------------------------- */

const CRC_TABLE = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(buf) {
  let c = -1;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // colour type: RGBA
// 10,11,12 = compression, filter, interlace: all 0

// One filter byte (0 = None) per scanline.
const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0;
  Buffer.from(px.buffer, y * SIZE * 4, SIZE * 4).copy(raw, y * (SIZE * 4 + 1) + 1);
}

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

const out = path.join(ROOT, "src-tauri", "icons", "source.png");
mkdirSync(path.dirname(out), { recursive: true });
writeFileSync(out, png);
console.log(`  wrote ${path.relative(ROOT, out)}  ${SIZE}x${SIZE}  ${(png.length / 1024).toFixed(1)} KB`);
console.log("  next: npx tauri icon src-tauri/icons/source.png");
