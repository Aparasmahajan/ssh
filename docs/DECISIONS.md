# DECISIONS.md

Deviations from `UI_BUILD_GUIDE.md` and `CLAUDE.md`, with reasons. The guide's
appendix asks for this file so future sessions stop re-litigating settled
choices. Append; don't rewrite history.

---

### D-1 — Frontend-first, against `CLAUDE.md` §17 build order

`CLAUDE.md` §17 is emphatic: build the terminal engine first, and "Do not start
step 6 until step 1's budgets are green in CI." That is the right order and it is
not being followed, for one reason only: **there is no Rust toolchain on this
machine**, so no terminal engine can be compiled, let alone benchmarked.

Consequence, stated plainly: every performance claim in `CLAUDE.md` §4 is
currently **unmeasured**. The frontend work being done first is the part §17
calls "UI chrome", which §17 explicitly says not to build before the budgets are
green.

Mitigation: nothing built in the frontend phases assumes the terminal is a DOM
element. The session surface is a positioned region that the wgpu surface will
occupy (guide Step 8), and no phase introduces an HTML terminal or xterm.js that
would later need ripping out.

Revisit as soon as `cargo` exists. **Amended by D-12**, which scaffolds the
Tauri project so only the compile step is missing.

---

### D-2 — Fuzzy host filtering lands in TypeScript first

Guide Step 5: "Fuzzy filtering happens in RUST, not JS." Blocked by D-1.

Plan: implement it behind a single async seam named exactly as the guide's Tauri
command — `filter_hosts(query, tags)` returning ordered host IDs plus match
ranges — so the Rust implementation replaces the body and no caller changes. The
TS version is a stopgap and is labelled as one in the source.

The 5ms-per-keystroke assertion in Step 5 still gets written and run against the
TS version. If TS passes it at 10,000 hosts, that is useful information; if it
fails, the guide's own note applies — profile before assuming the language is
the problem.

---

### D-3 — `tailwind.css` is imported *between* `fonts.css` and `tokens.css`

Guide Step 2 says: "Import fonts.css then tokens.css (in that order)." It does
not say where the Tailwind layers go.

They go in the middle. `tokens.css` ends with base rules on `html, body, #root`
(background, font-family, `overflow: hidden`), and those must win over Tailwind
Preflight. Same-specificity selectors resolve by source order, so `tokens.css`
must come last. The fonts-before-tokens order the guide asks for is preserved.

---

### D-4 — Status rail uses small-caps labels, not the emoji in the §0.5 sketch

The §0.5 ASCII layout sketch shows `🔒 chacha20-poly1305` and `⚡ 7 sessions`.
Emoji are outside every subset range listed in `fonts.css`, so they resolve to
the OS colour-emoji face — full-colour glyphs on a monochrome graphite chassis,
at a size where they read as noise.

Replaced with condensed uppercase eyebrow labels (`CIPHER`, `SESSIONS`), which is
already the design language ("small-caps labels", §0.9). The directional arrows
`↑ ↓` are kept: U+2190–21FF is explicitly in the subset ranges.

---

### D-5 — Zoom (`⌘⏎`) hides the rail, tree, and inspector — but keeps the status rail

§0.5 calls zoom "full bleed" and "the 'I'm debugging, get out of my way' state"
without enumerating what disappears.

The status rail stays. It is 28px of pure signal — RTT, throughput, cipher — and
a user who has just zoomed in to debug a session is precisely the user who wants
the RTT readout. Hiding it would be following the word "full" against the intent
of the sentence next to it.

---

### D-6 — Chrome layout state persists to `localStorage`, not the Tauri store

Guide Step 3 says "Persist panel widths and collapse state to the Tauri store".
Blocked by D-1.

`src/lib/persist.ts` is the seam, with a `opspilot:machine:` key prefix that
mirrors where this state belongs per `CLAUDE.md` §12.1 — the OS state dir's
`machine.toml`, the "DO NOT COPY. All rebuildable" side of the split. Swapping
the backend touches that one file.

Writes are debounced 250ms. A resize drag emits a value per `pointermove`;
writing storage synchronously from a drag handler is the UI-thread work
`CLAUDE.md` §13.1 rule 1 forbids.

---

### D-7 — Zoom state is not persisted

Not addressed by the guide. Panel widths and collapse flags persist; `zoomed`
does not. Restoring into full-bleed zoom on launch — no module rail, no tree, no
inspector — is indistinguishable from a broken window, and the user has no
visible affordance to recover other than a keybinding they may not recall.

---

### D-8 — Fixture routes instead of Storybook

Step 4 asks for "a Storybook story covering every status × environment × compact
combination". Built as a fixture route (`/?fixture=host-row-gallery`) instead.

`visual-check.spec.ts` already navigates to `/?fixture=session-view`, so
query-param fixtures are the harness's own entry mechanism. Reusing it means no
second build system, no ~40 extra dependencies against a 250ms cold-start budget
(CLAUDE.md §14.5 asks for a dependency-count audit), and the gallery is directly
screenshot-testable by the Playwright suite that already exists.

If component-level docs become valuable later, Storybook can be added without
removing this.

---

### D-9 — `filter_hosts` returns ranked ids and no highlight ranges

Step 5 says the command "returns ordered host IDs plus match ranges for
highlighting". It returns ordered ids and a matched-field marker; ranges come from
a separate `rangesFor(index, hostIdx, field, query)` called per visible row.

Highlighting exists only for rows on screen — about 30 of them. Computing ranges
for all 10,000 results is work whose output is discarded, and it was measurable:
allocating a ranges array per result was the dominant GC pressure in the p99
column of `scripts/bench-filter.ts`. Over the real Tauri boundary it would also be
10,000 arrays serialized to paint 30, which CLAUDE.md §3 ("IPC to webview:
shared-memory ring + rkyv. Never JSON") rules out anyway.

The guide's intent — ranges are computed once, never re-derived inside render — is
preserved. `fuzzyScore` and `fuzzyMatch` share one scorer so the two paths cannot
disagree about what matched.

---

### D-10 — The fzf scorer is v1, and stays v1 in TypeScript

CLAUDE.md §7 asks for fzf **v2**: bit-parallel prefilter, then Smith-Waterman
scoring. The prefilter is implemented as specified (as a u64, carried as two
32-bit words since JS bitwise ops are 32-bit). The scorer is fzf **v1** — greedy
forward pass, backward tighten, one scoring sweep with boundary/camel/consecutive
bonuses.

v1 is O(n) per candidate against v2's O(n·m), and the two differ only in ranking a
small number of ambiguous candidates. In TypeScript the DP would cost more than
the ranking improvement is worth; in Rust it is affordable. So the DP lands with
the port (D-2), and `src/lib/fuzzy.ts` says in its header not to "upgrade" the TS
version.

---

### D-11 — Row hover is ~2%, not chassis-700

§0.6: "Never a row hover that changes background lightness by more than 4% — it
flickers when you drag across a long list."

`--chassis-800` → `--chassis-700` is about 4.4% in L*, just over the cap. Hover is
therefore `color-mix(in srgb, var(--rail-600) 35%, transparent)` — roughly +2% —
defined as `.row-hoverable` in `src/styles/tailwind.css`.

`--chassis-700` is still used, for **selection**: tokens.css names it "selected
row", and a click is deliberate in a way that dragging past a row is not, so the
larger step is right there.

`color-mix` keeps the value derived entirely from tokens. The no-hex ESLint rule is
about provenance, not merely syntax, so satisfying it with a hand-computed hex
would have missed the point.

---

### D-12 — The Tauri project is scaffolded but never compiled

Amends D-1. Phases 1–3 built only the webview half, and the project contained no
Tauri artifacts at all — which made "a Tauri desktop app" a claim the repository
did not support. Phase 3.5 creates the workspace, `src-tauri`, and the six crates
so the structure is right and `cargo check` has something to check the moment a
toolchain exists.

The cost of doing this without a compiler is real and should not be understated:
every line of Rust in the tree is unverified. The mitigations were to keep the
Rust surface as small as possible (five of six crates are a `const` and a test),
add no external dependencies beyond `tauri` and `serde`, and write no stub whose
wrong answers could be mistaken for right ones.

What *was* verifiable got verified: `npx tauri info` parses `tauri.conf.json` and
reports the CSP, dist path, dev URL, and detected framework back correctly. The
Tauri CLI ships as a prebuilt binary through npm, so it runs without Rust.

**Outcome, once a toolchain existed:** five of the six crates, both profiles, the
toolchain pin and the workspace manifest compiled clean on the first try. The
`crates/app` + `src-tauri` pair did not — see D-13. Two real defects in roughly 200
lines of unverifiable Rust is about the rate you should expect from this approach,
which is the argument for not writing very much of it blind.

---

### D-13 — `crates/app` exposes `register(builder)`, not the commands

Originally `crates/app` exported `pub fn app_version` with `#[tauri::command]`, and
`src-tauri` named it inside its own `generate_handler!`. That does not compile:

```
error[E0255]: the name `__cmd__app_version` is defined multiple times
```

`#[tauri::command]` expands to helper macros named after the function plus a
`pub use` of them. Marking the function `pub` re-imports those macros into the same
module, and the macro namespace rejects the duplicate. Tauri's own template sidesteps
this by keeping command functions private and calling `generate_handler!` in the same
crate.

So `crates/app` now exposes one function:

```rust
pub fn register(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry>
```

`src-tauri` keeps only `generate_context!`, which must be invoked from the crate that
owns `tauri.conf.json`. This is better than the original regardless of the error: the
handler list now lives beside the commands, so adding a command touches one crate
instead of two and cannot be half-done.

---

### D-14 — The app icon is generated from tokens.css

`tauri-build` needs `src-tauri/icons/icon.ico` on Windows to emit a Resource file, so
a missing icon fails `cargo check --workspace` — not just `tauri build`, as
`src-tauri/icons/README.md` originally claimed and as `scripts/doctor.mjs` originally
scored it. Both are corrected.

Rather than commit an opaque binary, `scripts/make-icon.mjs` draws the source PNG and
`npx tauri icon` fans it out. It **parses the palette out of `src/styles/tokens.css`**
rather than hardcoding colours: tokens.css says a missing value should be added there
and never inlined, and an icon quietly drifting from the palette is exactly the decay
the no-hex rule exists to stop.

The mark is the product thesis at icon scale — a faceplate, the amber environment tie
bar, port chips, and exactly one cyan link LED (§0.2 permits cyan for nothing else).

---

### D-15 — The viewport grid is a ring of rows

`CLAUDE.md` §5.1 mandates flat structure-of-arrays planes and forbids
`Vec<Vec<Cell>>`. It does not say how scrolling works, and the obvious reading —
`copy_within` over the planes — is a trap.

Measured, not suspected: `benches/term.rs` put the parser at 400 MiB/s standalone
and 7.45 MiB/s once the grid was attached. A 4 MiB `cat` is ~75,000 line feeds, each
memmoving a 50x200 grid, so 4.5 GB of memmove to display 4 MiB.

Logical row `r` now lives at physical row `(r + row_offset) % rows`. Scrolling
increments the offset and blanks one row. Result: +530% on plain ASCII, +467% on
SGR-heavy, both measured by Criterion.

This keeps every §5.1 property. Rows remain contiguous, so the stride within a row
is unchanged and there is still no pointer chase — the offset costs one add and one
predictable compare per row lookup, not an indirection.

Consequence for callers: `Grid` exposes `row(n)`, never a whole plane. With the
offset the raw `Vec` is not in logical order, and handing it out would invite
someone to iterate it as if it were.

---

### D-16 — `memchr` is not used for the ASCII bulk path

§5.2 says to "use `memchr` to find the next byte `< 0x20 || == 0x7F`". `memchr`
finds *specific bytes*; this is a *range* predicate, and expressing it as a byte-set
search would mean scanning for 33 separate values.

`parser::ascii_run_len` does it with two SWAR bit tricks over `u64` words —
`hasless(w, 0x20)` and `hasmore(w, 0x7E)` — falling back to a byte loop on the word
that contains the first offender, because the borrow in `hasless` can set bits in
lanes after it. Measured at **821 MiB/s**.

This also keeps `core-term` free of runtime dependencies, which §16 asks for and
§14.5 rewards: nothing to link, nothing to initialise at startup.

---

### D-17 — Predictions are an overlay, not a second grid

§5.5 specifies two grids:

```
authoritative_grid   ← only ever mutated by bytes from the server
predicted_grid       ← authoritative + locally predicted edits, THIS is what renders
```

Implemented instead as a small list of predicted cell edits, applied on top of the
authoritative grid at read time.

The reason is that the gating rules make a prediction very small. Everything §5.5
permits — printable ASCII, backspace, horizontal cursor movement — touches **at
most one cell plus the cursor**. So the overlay is complete: there is no predicted
state a second grid could represent that `(row, col, glyph, style)` plus a cursor
override cannot.

What the second grid would cost: a full copy of the grid on every server read to
rebuild `predicted` from `authoritative` (60 KB for a 200x50 viewport), plus a
duplicate scrollback, plus the resize/reflow logic maintained twice.

The specified *semantics* are unchanged, including the one that matters most —
"discard **all** outstanding predictions and snap" on a mismatch, which here is
clearing the overlay, O(predictions) rather than O(cells).

Consequence for the renderer: it draws the authoritative grid, then applies at most
12 overlay cells and an optional cursor override. That is also why `Predictor`
exposes `overlay()` as an iterator rather than a per-cell lookup — a per-cell query
would be O(cells x predictions) across a frame, which is the shape of cost §5.4
exists to avoid.

---

### D-18 — The predictor takes a clock instead of reading one

Timeouts (`PREDICTION_TIMEOUT_MS`, and the silent-timeout count that infers a
password prompt) need a monotonic clock. `Predictor` takes `now_ms: u64` as a
parameter rather than calling `std::time::Instant::now()`.

Two reasons. It keeps `core-term` dependency-free and free of ambient state, per
§14.5. And it makes the timing tests deterministic: the echo-off inference and the
hit-rate cutoff are tested by passing explicit timestamps, so they cannot flake on
a loaded CI machine — which for a security-relevant behaviour (not painting
characters at a password prompt) matters more than the convenience of an implicit
clock.

---

### D-19 — The terminal renders to a 2D canvas, and this is temporary

`CLAUDE.md` §3: "Anything updating faster than 4 Hz is a wgpu surface."
UI_BUILD_GUIDE Step 8: the terminal "must NOT be an HTML element or xterm.js".
`src/components/TerminalSurface.tsx` is a 2D canvas in the webview. It complies
with neither.

Why anyway: `crates/render` does not exist, and without it the app has a window, a
host tree, a status rail and no terminal. Something that runs a shell today is worth
more than a placeholder that is architecturally pure — provided the debt is named
here rather than discovered later.

What is preserved, because these parts survive the port to wgpu unchanged:

- **The webview never sees the byte stream.** PTY → parser → grid all happen in
  Rust. The webview receives changed rows, already resolved to text and style spans.
- **Damage-driven.** Only rows the Rust side reports dirty are repainted.
- **Coalesced at vsync (§5.4).** One snapshot per animation frame regardless of how
  many thousands of PTY writes arrived — Rust bumps a counter, the UI samples it
  (§13.1 rule 8: "Never one message per chunk").

What it costs, precisely: glyphs are rasterised by the browser on the UI thread, so
a flood competes with React for that thread. That is the exact coupling §3 exists to
forbid, and it is why the §4 rows for keystroke latency and 200 MB `cat` **cannot be
claimed** from this renderer. The engine behind it measures 47 MiB/s; the ceiling
here is the canvas, not the parser.

Replacing it should touch one component — nothing above `TerminalSurface` knows how
pixels get drawn.

---

### D-21 — IPC sends rows as text plus style spans, not cells, and not rkyv

§3: "IPC to webview: shared-memory ring + rkyv. Never JSON." Tauri v2's `invoke` is
JSON, and a shared-memory ring into a webview is not something Tauri offers.

The mitigation is to make the payload small enough that the encoding stops
mattering. A row crosses as one string plus its style runs — a typical line is one
or two spans, against 200 per-cell objects. Combined with sending only dirty rows,
and returning `null` when nothing changed at all, an idle terminal serialises
nothing per frame and a busy one sends a few KB.

This dies with the canvas (D-19): once the terminal is a wgpu surface, the grid
never crosses the IPC boundary at all.

---

### D-22 — the vault has no OS-keychain integration at all, not even optional

§12.3 permits one: the keychain may hold a **wrapped copy** of the derived key as
a local biometric convenience, absent on a new machine, re-wrapped after a password
unlock. That design is sound, and it is not implemented.

The reason is that the same section calls the failure mode permanent — make DPAPI
the root of trust and the vault "becomes permanently unopenable on any other
machine, with no recovery path" — and a wrap is exactly the code path that turns
into a root of trust the first time someone simplifies it. With no keychain code
present, `Vault::open(path, password)` has nothing machine-local to reach for, and
`a_vault_opens_on_a_different_machine` fails the moment that changes.

Cost: the master password is typed at every unlock (15-minute idle timeout). That is
the right direction to be incomplete in — adding the convenience later is additive
and testable; recovering from an unopenable vault is not possible at all.

---

### D-23 — no command returns a secret, in place of §12.3's broker process

§12.3 puts the secrets broker in a separate process so "the UI receives a handle,
not plaintext, so a webview XSS cannot read the vault". Tauri v2 has no
supported multi-process split of this kind, and building one over a hand-rolled IPC
channel would be a large amount of security-critical surface written from scratch.

Instead the boundary is the command list: nothing registered in `generate_handler!`
returns plaintext, and the only decrypting function is `pub(crate)`. Authentication
is by reference — the webview sends `vault:prod/deploy` and never sees the value.
`no_command_returns_a_secret` parses the handler list and fails if a
`vault_get`-shaped command is ever added.

What this does **not** cover, and the broker would: a compromise of the native side.
The threat §12.3 names by name — webview XSS — is covered; the broader one is not.

Replacing it later touches `crates/app/src/vault.rs` and nothing above it: the
frontend already speaks in references rather than values, which is the part that
would otherwise have to change.
