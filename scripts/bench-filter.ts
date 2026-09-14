/**
 * Step 5: "Generate a 10,000-host fixture and add a test asserting that
 * filter-to-repaint stays under 5ms."
 *
 * SCOPE, stated plainly: this measures the FILTER half only — `filter_hosts`
 * from keystroke to ranked results. The repaint half needs a browser and is not
 * measured (see COMPLETED.md). Calling this "filter-to-repaint" would be exactly
 * the unmeasured speed claim CLAUDE.md §16 forbids.
 *
 * Two tables, because they answer different questions:
 *
 *   COLD  — no prefix reuse. The pessimistic bound: what a paste, a cleared
 *           field, or a first keystroke costs.
 *   TYPED — character by character with the previous result fed back in, which
 *           is what a human at a keyboard actually produces (CLAUDE.md §7:
 *           "Cache scores... so each extra character reuses prior work").
 *
 * Run:  node scripts/bench-filter.ts
 * (Node 24 strips TypeScript types natively — no build step.)
 */
import { buildHostIndex, filterHostsSync, type FilterResult } from "../src/lib/filterHosts.ts";
import { generateHosts } from "../src/fixtures/hosts.ts";

const N = 10_000;
const BUDGET_MS = 5;
const ITERATIONS = 300;

/** "10." is the pathological case on purpose: every synthetic address starts
 *  with it, so nothing is rejected and 10,000 candidates all match. */
const COLD_QUERIES = ["p", "pg", "pg-eu", "pg-eu-1", "k", "kaf", "10.", "10.20", "zzz"];
const TYPED_PHRASES = ["pg-eu-01", "10.20.4", "kafka"];

const hosts = generateHosts(N);

const t0 = performance.now();
const index = buildHostIndex(hosts);
const buildMs = performance.now() - t0;

// Warm up so the JIT has tiered up. A cold first call is not what a user's
// second keystroke costs, and reporting it as such would flatter nothing.
for (let i = 0; i < 100; i++) for (const q of COLD_QUERIES) filterHostsSync(index, q);

const pct = (xs: number[], p: number) => xs[Math.min(xs.length - 1, Math.floor(xs.length * p))] as number;
const fmt = (n: number) => n.toFixed(3).padStart(8);

let worstP99 = 0;
let worstLabel = "";

function record(label: string, samples: number[], results: number) {
  samples.sort((a, b) => a - b);
  const p50 = pct(samples, 0.5);
  const p99 = pct(samples, 0.99);
  if (p99 > worstP99) {
    worstP99 = p99;
    worstLabel = label;
  }
  const flag = p99 > BUDGET_MS ? "  ✗ OVER" : "";
  console.log(`  ${label.padEnd(12)}${fmt(p50)}${fmt(p99)}${String(results).padStart(10)}${flag}`);
}

console.log(`\n  filter_hosts — ${N.toLocaleString()} hosts, ${ITERATIONS} iterations each`);
console.log(`  index build: ${buildMs.toFixed(1)} ms (once per config load, never per keystroke)`);

console.log(`\n  COLD — no prefix reuse`);
console.log(`  ${"query".padEnd(12)}${"p50 ms".padStart(8)}${"p99 ms".padStart(8)}${"results".padStart(10)}`);
for (const query of COLD_QUERIES) {
  const samples: number[] = [];
  let results = 0;
  for (let i = 0; i < ITERATIONS; i++) {
    const start = performance.now();
    results = filterHostsSync(index, query).count;
    samples.push(performance.now() - start);
  }
  record(query, samples, results);
}

console.log(`\n  TYPED — each keystroke reuses the previous result`);
console.log(`  ${"keystroke".padEnd(12)}${"p50 ms".padStart(8)}${"p99 ms".padStart(8)}${"results".padStart(10)}`);
for (const phrase of TYPED_PHRASES) {
  for (let len = 1; len <= phrase.length; len++) {
    const prefix = phrase.slice(0, len);
    // Build the chain ONCE, outside the timing loop, then hold it fixed. An
    // earlier version rebuilt it per iteration, which allocated result arrays
    // for every intermediate prefix and charged that garbage collection to the
    // one call being measured. Measuring the harness instead of the code is an
    // easy mistake to make and a hard one to notice.
    let prev: FilterResult | undefined;
    for (let k = 1; k < len; k++) prev = filterHostsSync(index, phrase.slice(0, k), [], prev);

    const samples: number[] = [];
    let results = 0;
    for (let i = 0; i < ITERATIONS; i++) {
      const start = performance.now();
      const r = filterHostsSync(index, prefix, [], prev);
      samples.push(performance.now() - start);
      results = r.count;
    }
    record(prefix, samples, results);
  }
  console.log("");
}

console.log(`  worst p99: ${worstP99.toFixed(3)} ms on "${worstLabel}" — budget ${BUDGET_MS} ms`);
if (worstP99 > BUDGET_MS) {
  console.error("  FAIL: filter exceeds the Step 5 budget.\n");
  process.exit(1);
}
console.log("  PASS\n");
