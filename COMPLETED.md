# COMPLETED.md — OpsPilot build log

Running record of what has actually been built, phase by phase. Rules for this
file: only claim what was **verified**, name the verification, and list what was
deliberately left out.

Reference docs: [`CLAUDE.md`](CLAUDE.md) (performance constraints),
[`UI_BUILD_GUIDE.md`](UI_BUILD_GUIDE.md) (step plan),
[`docs/DECISIONS.md`](docs/DECISIONS.md) (deviations from the guide and why).

---

## Environment as found

| Tool | State | Consequence |
|---|---|---|
| node 24.14.0 / npm 11.11.0 | present | frontend builds |
| **cargo / rustc** | **absent** | the Rust workspace and Tauri shell (Step 1, back half) **cannot be built or verified**. Deferred, not skipped — see Open items. |
| **Python fontTools** | **absent** | fonts cannot be subsetted from full TTFs; see Open item 1 |
| Playwright browsers | not installed | the screenshot/DOM tests in `visual-check.spec.ts` cannot execute yet |

Because of the missing Rust toolchain the build order is **frontend-first**:
`UI_BUILD_GUIDE.md` Steps 2–7 and 10–13 are almost all frontend, so this costs
little. The step that genuinely blocks on Rust is Step 8 (wgpu terminal
surface), which `CLAUDE.md` §17 puts first for good reason — that reordering is
recorded in `docs/DECISIONS.md` as D-1.

---

## Phase 1 — Setup (guide Steps 1–2)

Status: **frontend half complete and verified. Desktop half was missing entirely
until Phase 3.5 — see the correction below.**

> **Correction.** This section originally read "complete and verified", with the
> Rust half listed as a deferred item further down. That was misleading: OpsPilot
> is a Tauri desktop app, and for Phases 1–3 there was no `src-tauri/`, no
> `Cargo.toml`, no `crates/`, and no `@tauri-apps` dependency anywhere in the
> project. It was a browser app that resembled the target UI. Phase 3.5 scaffolds
> the desktop project; it still cannot be compiled.

### Created

| Path | Purpose |
|---|---|
| `package.json` | React 18 + TS strict + Vite 5 + Tailwind 3 + zustand 5 + `@tanstack/react-virtual` 3, per Step 1 |
| `tsconfig.json` | strict mode, plus `noUncheckedIndexedAccess` and `exactOptionalPropertyTypes`; `@/*` → `src/*` |
| `vite.config.ts` | React plugin, `@` alias, `target: chrome120` (Tauri ships a known webview — no legacy transpile tax) |
| `postcss.config.js` | tailwind + autoprefixer |
| `index.html` | single `#root`, `color-scheme: dark` |
| `src/main.tsx` | load order `fonts.css` → `tailwind.css` → `tokens.css` |
| `src/styles/tailwind.css` | the three `@tailwind` layers, nothing else |
| `src/styles/tokens.css` | **copied verbatim** from the provided `tokens.css` |
| `src/styles/fonts.css` | **copied verbatim** from the provided `fonts.css` |
| `tailwind.config.ts` | **already at project root**, used as provided — no edits |
| `tests/visual/visual-check.spec.ts` | **copied verbatim** from the provided spec |
| `playwright.config.ts` | `baseURL` 5173, baselines at `tests/visual/baseline/`, auto-starts the dev server |
| `eslint.config.js` | flat config; **the no-hex-literal rule** |
| `scripts/vendor-fonts.mjs` | copies woff2 out of `@fontsource` into `src/styles/fonts/` on `postinstall` |
| `.gitignore`, `.claude/launch.json` | |

The three provided files were **copied, not moved** — `tokens.css`, `fonts.css`,
and `visual-check.spec.ts` still sit at the repo root. Those root copies are now
stale duplicates and should be deleted; they were left in place rather than
deleted unasked.

### Verified

- `npm run typecheck` — clean.
- `npm run lint` — clean.
- `npm run build` — succeeds; all seven woff2 faces are emitted into `dist/assets/`,
  so there is **no network font request** (offline-first, `fonts.css` header).
- **The no-hex rule actually fires.** Probed with a throwaway file: it flags both
  `"#1B1F26"` and a template literal `` `1px solid #F2A93B` ``, and correctly
  ignores the non-colour string `"#region"`. An unenforced lint rule is worse
  than no rule, so this was tested rather than assumed.
- Every token-derived Tailwind utility reaches the output CSS — `w-rail`,
  `w-inspector`, `h-status`, `h-tabstrip`, `text-eyebrow`, `border-hairline`,
  `duration-hover`, `bg-chassis-800`, `text-text-lo`, `font-condensed` all
  present in the built stylesheet. Worth checking because the config *replaces*
  `fontSize`/`fontFamily`/`borderRadius` rather than extending them.
- Dev server serves the app, the TS entry module, and the vendored woff2 (all 200).

### NOT done in this phase

- **`npx playwright test visual-check` has not been run.** It needs a Chromium
  download (~120 MB from Microsoft's CDN) which was not authorised. So the
  "design system invariants" block that Step 2 asks for — including the
  `document.fonts.check()` assertion, the highest-impact fidelity check in the
  whole build — is **written and wired but never executed**.
- Font subsetting per the `pyftsubset` command in the `fonts.css` header. See
  Open item 1.
- The Rust workspace crates (`core-term`, `core-ssh`, `core-sftp`, `core-store`,
  `render`, `app`) and the Tauri shell.

---

## Phase 2 — Shell layout (guide Step 3)

Status: **complete and verified** (statically; see caveat).

### Created

| Path | Purpose |
|---|---|
| `src/components/AppShell.tsx` | the four zones + status rail; global keybindings |
| `src/components/ModuleRail.tsx` | zone 1 — 48px, icon-only, 3px amber active edge |
| `src/components/StatusRail.tsx` | zone 5 — 28px, RTT / throughput / cipher / sessions / palette hint |
| `src/components/Resizer.tsx` | 4px rail, 12px hit area, pointer + keyboard |
| `src/components/Tooltip.tsx` | explicit 600ms hover delay |
| `src/components/Placeholders.tsx` | stand-ins for tree / surface / inspector |
| `src/state/layout.ts` | zustand store, panel state persisted **per workspace** |
| `src/lib/persist.ts` | storage seam — localStorage now, Tauri store later |
| `src/lib/platform.ts` | ⌘ vs Ctrl resolved once, for handlers *and* hint labels |

### Behaviour built to spec

- Module rail 48px fixed; workspace tree 260px default, **clamped 200–480**;
  session surface flex; inspector 280px; status rail 28px full width.
- Resize handle is **4px visible with a 12px hit area** — an absolutely
  positioned overlay bleeding 4px past each edge, so entering the hit area
  shifts no layout.
- The handle is **keyboard-resizable** (`role="separator"`, arrows ±8px, Shift
  ±32px, Home/End to the clamps). §0.8 says every action is reachable without a
  mouse, and a drag handle is the easiest place to forget that.
- Panel widths and collapse state **persist per workspace**, debounced 250ms —
  a drag emits a value per `pointermove`, and writing storage from a drag
  handler is the UI-thread work `CLAUDE.md` §13.1 rule 1 forbids.
- `⌘B` tree, `⌘I` inspector, `⌘⏎` zoom, `Esc` exits zoom. Registered on `window`
  in the **capture phase** so the shell wins over any pane that later grabs
  keys — a terminal will want nearly every other key, but not these three.
- Inspector **starts collapsed under 1600px** (§0.5), as a default only: an
  explicit user choice is persisted and wins.
- Tooltips at a real 600ms. Not `title=""`, whose delay is browser-controlled,
  unstylable, and absent on keyboard focus — and the rail is icon-only, so that
  label is the only thing naming the control.
- `aria-label` on every rail button; `aria-current` on the active one.
- Zoom is **not persisted** — it is a momentary mode, and restoring into it with
  no visible tree would read as a broken window.

### Verified

- `npm run typecheck` and `npm run lint` clean.
- Dev server compiles and hot-reloads with no errors.

### Caveat on verification

**No interaction was exercised in a real browser.** The Browser pane refuses
`localhost` under policy and Playwright's Chromium is not installed, so drag
resize, the keybindings, tooltip timing, focus rings, and the 4.5:1 contrast
floor are **built to spec but unproven**. Treat every Phase 2 behaviour as
"written correctly as far as static analysis can tell". This is the single
biggest gap in the log so far.

### NOT done in this phase

- Persisting to the **Tauri store** (Step 3 says Tauri store; there is no Tauri
  yet). `src/lib/persist.ts` is the seam — swapping it touches one file.
- Real contents for the three zones (Phases 3–5).

---

## Phase 3 — Host row and virtualized tree (guide Steps 4–5)

Status: **complete.** Filter path **measured**; UI unverified in a browser.

### Created

| Path | Purpose |
|---|---|
| `src/types.ts` | `Host`, `Connection`, `Environment`, `HostStatus`, `ConnectionKind` |
| `src/components/HostRow.tsx` | the faceplate, to §0.6 + the §0.9 override |
| `src/components/PortChip.tsx` | one connection type, individually clickable |
| `src/components/WorkspaceTree.tsx` | virtualized tree, search, grouping |
| `src/lib/fuzzy.ts` | u64 bit-parallel prefilter + fzf-v1 scorer |
| `src/lib/filterHosts.ts` | the `filter_hosts(query, tags)` seam |
| `src/lib/truncate.ts` | middle truncation on a mono character budget |
| `src/fixtures/hosts.ts` | curated demo hosts + deterministic generator |
| `src/fixtures/HostRowGallery.tsx` | Step 4's full state matrix |
| `scripts/bench-filter.ts` | the Step 5 budget benchmark |

### Built to spec

- **Row height is 64px three-line, not 44px two-line.** §0.9 says Figma is
  authoritative and the guide is stale here: "the port chips *are* the product
  thesis and hiding them defeats the point. Follow Figma." Compact mode (26px,
  chips hidden) is also built, because §0.6 is right that 200-host users need it.
- 3px environment tie bar, full row height, `data-testid="env-tie"` so the
  fractional-DPI test can find it. Environment colour appears **nowhere else** on
  the row.
- Status dot 6px: filled cyan connected, hollow idle, filled red failed, amber
  pulsing 1.2s connecting. Cyan appears here and only here — as link state.
- Port chips: 11px mono uppercase, 3px radius, bezel hairline border, transparent
  fill, hover to amber-900 fill + amber-500 border and text. Each is its own
  click target and stops propagation, so a chip opens a connection rather than
  merely selecting the row.
- `SFTP·deploy` identity naming, always. Read-only identities are marked `·ro`
  rather than hidden.
- Address line middle-truncates. This is exact rather than approximate: the line
  is Plex Mono, so `chars = width / advance` holds. `src/lib/truncate.ts` warns
  against reusing it for proportional text.
- **Row hover is ~2% lightness, not 4.4%.** §0.6 caps hover at 4%, and
  chassis-800 → chassis-700 is 4.4% — just over. Hover is a 35% `color-mix` of
  rail-600 instead; chassis-700 is kept for *selection*, where tokens.css names it
  "selected row" and a deliberate click earns the larger step.
- Tree flattens to `VisibleNode[]`; render never recurses. Only visible rows
  mount. Query debounced one frame, previous query aborted, matched characters
  highlighted in amber from returned ranges.
- Keyboard: one tab stop per row, ← → between chips, Enter activates, ↑ ↓ across
  the flattened list with `scrollToIndex`. Chips are `tabIndex={-1}` on purpose —
  without that, tabbing through a 10,000-row tree would cross four chips per host.

### Measured — `node scripts/bench-filter.ts`

10,000 hosts. Budget 5ms (Step 5). Index build 36–66ms, once per config load.

| | worst p50 | worst p99 |
|---|---|---|
| First run | 218 ms | 839 ms |
| Current | **3.5 ms** | 12.3 ms |

**Every p50 is now inside the 5ms budget. p99 is not, and the script still exits
non-zero because of it.** That is deliberate: the gate was left at p99 rather than
relaxed to p50 to make the number look green.

Where the 240× came from — all four were structural, per CLAUDE.md §1:

1. **A 10,000-entry `Map` rebuilt per keystroke**, plus `localeCompare` in the
   sort comparator. This was the single biggest cost. Fixed by carrying the host
   index in the result and comparing precomputed lowercase names by codepoint.
   `localeCompare` also varies by platform locale, which would make an exported
   config sort differently on another machine.
2. **Array-of-structs results → structure-of-arrays** (CLAUDE.md §5.1). 10,000
   match objects per keystroke became two typed arrays. The `scores` array was
   then dropped entirely once it turned out nothing downstream read it.
3. **A polymorphic type annotation.** The candidate list was typed
   `Int32Array | readonly number[]`, which makes every indexed load polymorphic
   and un-inlinable. Changing the declaration to `Int32Array` alone was worth
   ~10× on the all-match queries. No logic changed.
4. **One bit of prefilter width.** The mask was 31 bits, so all ten digits shared
   one bucket — and since every hostname ends in an ordinal, the query `"1"`
   passed every host and got scored twice (name, then address). CLAUDE.md §7 says
   *u64*; widening to two 32-bit words made digits separable. The profiler put
   that lost bit at 5.2ms of a 5ms budget.

Also **found a bug in the benchmark itself**: the TYPED table rebuilt the whole
prefix chain inside the timing loop, so garbage from every intermediate prefix was
charged to the one call being measured. Fixed by building the chain once outside.

What remains over budget is one narrow case: a query matching a large fraction of
all hosts (`"1"`, `"10."` → 6,968–10,000 of 10,000 results), where p99 is 7–12ms
under back-to-back load. Two honest caveats: p99 here is dominated by GC of the
exact-sized result arrays at an allocation rate no human typing can produce, and
this is precisely the gap the Rust port exists to close (D-2). Typing a *narrowing*
query costs 0.001–0.8ms per keystroke.

### NOT verified

- **No browser.** Virtualized scrolling, keyboard navigation, hover thresholds,
  highlight rendering, and truncation have not been seen running. The filter is
  measured; the *repaint* half of Step 5's "filter-to-repaint under 5ms" is not,
  and the benchmark says so in its own header rather than implying otherwise.
- Tag filtering has an implementation and no UI; `filter_hosts` is always called
  with `tags: []`. A tag/env filter control isn't specified in the guide beyond
  the command signature.
- Compact mode has no in-app control yet (`?compact=1` only) — it belongs to the
  Settings module.

---

## Phase 3.5 — Desktop project scaffold (guide Step 1, back half)

Status: **five of six crates compiler-verified. `crates/app` and `src-tauri`
still uncompiled — blocked on the MSVC linker only.**

Prompted by a direct question — "aren't we making a desktop app here?" — which was
correct. Phases 1–3 had produced a browser app.

### Created

| Path | Purpose |
|---|---|
| `Cargo.toml` | workspace: `crates/*` + `src-tauri`, and the §3 release profile |
| `rust-toolchain.toml` | pins stable, with rustfmt + clippy |
| `src-tauri/Cargo.toml`, `build.rs`, `src/main.rs` | the desktop binary |
| `src-tauri/tauri.conf.json` | 1600×1000 dark window, dev server on 5173, locked CSP |
| `src-tauri/capabilities/default.json` | minimal `core:default` only |
| `src-tauri/icons/README.md` | the `tauri icon` command; icons not generated |
| `crates/{core-term,core-ssh,core-sftp,core-store,render}` | engine skeletons |
| `crates/app` | Tauri commands; one real command, `app_version` |
| `scripts/doctor.mjs` | preflight that names the missing prerequisites |
| `src/lib/runtime.ts` | `IS_DESKTOP`, and an `invoke` of `app_version` |

### Decisions inside the scaffold

- **Release profile is the one `CLAUDE.md` §3 fixes**: `lto = "fat"`,
  `codegen-units = 1`, `panic = "abort"`, `opt-level = 3`. Dev builds get
  `opt-level = 1` with dependencies at 3 — a VT parser and cipher path compiled
  at 0 make a debug session unusable.
- **The engine crates have no dependencies and are not wired into `crates/app`.**
  §3 requires them behind feature flags and lazy-loaded; adding `russh`, `wgpu`
  and `arrow` as unconditional dependencies now would make all of them resident
  at startup, against a 250ms cold-start budget, for code nothing calls. The
  wiring is present as commented-out optional deps.
- **No `filter_hosts` stub.** A command that compiles and returns nothing is worse
  than one that does not exist, because the frontend would bind to it and silently
  show an empty tree. `crates/app/src/lib.rs` instead carries a porting note
  listing what to bring across from the measured TS version and what to add.
- **The window states its own shell.** In a browser the status rail shows a red
  `browser · no terminal` chip. Red because `--red-500` is "fault" and a shell
  that cannot open a session is a fault; amber is the accent and cyan is
  link-state only (§0.2).
- CSP is explicit rather than `null`, and the capability set is `core:default`
  only — §12.3's argument for a separate secrets broker process is undermined by a
  permissive webview.

### Verified

- `npx tauri info` **parses `tauri.conf.json`** and reports back the CSP,
  `frontendDist`, `devUrl`, and correctly detects the React/Vite frontend. The
  config is well-formed; that much is real.
- `npm run doctor` reports the same two blockers the Tauri CLI does.
- `npm run typecheck`, `lint`, and `build` still clean with `@tauri-apps/api`
  imported.

### Toolchain installed, and what that proved

`rustup 1.29.0` installed via winget; `rustc` / `cargo` **1.97.1**.

One wrinkle worth recording: the first install left a **partially installed
toolchain**, because the background task running it was stopped mid-write. It
self-repaired on next use (`recovering from a partially installed toolchain`,
then re-downloading all six components). Nothing was corrupted, but it is why the
first `rustc --version` failed after a "successful" install — a stopped installer
is not the same as a failed one.

Verified with the toolchain:

| Check | Result |
|---|---|
| `rust-toolchain.toml` | **honored** — pulled rustfmt + clippy exactly as pinned, and overrides the global default from inside the project |
| Workspace `Cargo.toml` | **valid** — 418 packages resolved into `Cargo.lock` |
| `[profile.dev] opt-level = 1` | **in effect** — cargo reports `dev profile [optimized + debuginfo]` |
| `cargo check` on core-term, core-ssh, core-sftp, core-store, render | **clean** |
| `cargo clippy --all-targets -- -D warnings` on those five | **clean** |
| `cargo fmt --all --check` (whole workspace) | **clean** |

So five of the six crates written without a compiler are now compiler-verified,
along with the manifests and both profiles.

### Still NOT verified

`crates/app` and `src-tauri` have never compiled. `cargo check --workspace` gets
as far as building proc-macro dependencies and then stops:

```
error: linker `link.exe` not found
note: the msvc targets depend on the msvc linker but `link.exe` was not found
```

Which means these remain unchecked: `tauri::generate_handler!`,
`tauri::generate_context!` reading `tauri.conf.json`, the `app_version` command,
and every `#[tauri::command]` signature. Proc macros must be *compiled and linked*
as dynamic libraries, so even `cargo check` needs the linker for that subtree —
`--workspace` cannot get past it.

An attempt to install the Build Tools returned **1602 (user cancelled)**: the shell
here is not elevated, winget handed off to the VS installer, and the UAC prompt was
not accepted. It left nothing behind — no `vswhere`, no `link.exe`, no winget
package entry — so the failure was clean and there is nothing to undo.

WebView2 150.0.4078.105 **is** present, so that link in the chain is already done.

```bash
npm run doctor
```

---

## Phase 4 — Terminal engine core (`CLAUDE.md` §5, §17 step 1)

Status: **complete and measured.** 50 tests, clippy `-D warnings` clean, fmt clean.

With a toolchain available, D-1's promise comes due: the build order reverts to
`CLAUDE.md` §17, which puts the terminal engine first and says to prove its
budgets before more UI chrome. This is that.

### Created — `crates/core-term`

| Module | Implements |
|---|---|
| `style.rs` | style interning — the reason a cell is 6 bytes, not 16 (§5.1) |
| `grid.rs` | SoA planes over a **row ring**, O(1) damage bitset (§5.1, §5.3) |
| `params.rs` | fixed-capacity CSI parameter accumulator, no allocation |
| `parser.rs` | VT500 DFA as a `const fn` table + SWAR ASCII bulk path (§5.2) |
| `scrollback.rs` | fixed-capacity ring, trailing blanks not stored (§5.6) |
| `screen.rs` | the `Perform` that turns parser actions into cell writes |
| `benches/term.rs` | Criterion, per §16 |

Still no runtime dependencies. §5.2 suggests `memchr`; it is not used, because
memchr searches for *specific bytes* and the predicate here is a *range*
(`< 0x20 || >= 0x7F`). That is SWAR over `u64` words instead — no crate, no
`unsafe`, nothing added to cold start.

### Measured — `cargo bench -p opspilot-core-term`

4 MiB corpora, 200x50 viewport, one core.

| Benchmark | Throughput |
|---|---|
| `ascii_run_len` (SWAR scan alone) | **821 MiB/s** |
| `parse_only` / plain ASCII | **400 MiB/s** |
| `parse_only` / SGR-heavy | 74 MiB/s |
| `parse_only` / box drawing | 55 MiB/s |
| `parse_only` / mixed UTF-8 | 42 MiB/s |
| `parse_and_grid` / plain ASCII | **47.0 MiB/s** |
| `parse_and_grid` / SGR-heavy | 32.3 MiB/s |
| damage scan, clean 100k rows | 2.1 µs |
| damage scan, 20 dirty of 100k | 3.2 µs |

The damage numbers confirm §5.3's claim directly: scanning 100,000 rows and
finding nothing costs 2.1 µs, because it is 1,563 word comparisons rather than
100,000 row checks.

### The benchmark immediately found a design defect

First run: `parse_only` 400 MiB/s but `parse_and_grid` **7.45 MiB/s**. Attaching
the grid cost 53x — not a constant factor, a defect.

The arithmetic identified it without a profiler. A 4 MiB `cat` is ~75,000 line
feeds; `scroll_up` was `copy_within` over the whole plane, so 75,000 x 60 KB =
**4.5 GB of memmove to display 4 MiB**. At ~10 GB/s that is 0.45s, against 0.54s
measured — close enough to be certain.

Fix, per §1 ("ask whether the *protocol or architecture* is wrong before tuning
the loop"): the viewport became a **ring of rows**. Logical row `r` lives at
physical row `(r + row_offset) % rows`; scrolling increments the offset and blanks
one row, O(cols) instead of O(rows·cols). Rows stay contiguous, so the SIMD stride
within a row is unchanged and §5.1's "never a pointer chase per row" still holds —
the cost is one add and one predictable compare per row lookup.

Criterion measured the change itself:

| | before | after | delta |
|---|---|---|---|
| `parse_and_grid` / plain ASCII | 7.45 MiB/s | 47.0 MiB/s | **+530%** |
| `parse_and_grid` / SGR-heavy | 5.69 MiB/s | 32.3 MiB/s | **+467%** |

One API consequence worth knowing: there is no accessor for a whole plane any
more, only `Grid::row(n)`. With the offset, the raw `Vec` is not in logical order,
and exposing it would invite a caller to iterate it as though it were.

### Correctness details that are easy to get wrong, and are tested

- **Deferred wrap.** Printing in the last column parks the cursor and sets a
  pending flag rather than wrapping immediately; `CR` cancels it. Without this an
  80-char line in an 80-column terminal leaves a spurious blank row.
- **C1 controls are not interpreted.** In UTF-8 those bytes are continuations —
  honouring the classic "anywhere" transitions would let the second byte of `œ`
  (0x9C) terminate a string and corrupt any non-ASCII output.
- **Colon is a parameter separator.** The classic table sends `:` to CSI-ignore,
  which would discard `SGR 38:2::R:G:B`. Both truecolour forms now parse
  identically, and a test asserts they produce the same style.
- **UTF-8 split across reads.** A PTY read can end mid-character; the decoder is
  incremental, and a test splits `é✓` at every byte boundary.
- **The alternate screen is not scrollback.** vim and htop repaint constantly;
  capturing those frames would evict real history from the ring.
- **Style interning saturates** rather than overflowing `u16` when a program
  emits a fresh truecolour per cell.

### NOT done

- **Reflow on resize** (§5.6). Resizing preserves the top-left and clamps the
  cursor; it does not re-wrap logical lines. That needs logical-line tracking
  above the grid.
- **Predictive local echo** (§5.5) — §17 step 3, and it needs a transport to
  reconcile against. The hooks it will need exist: `Parser::in_string()`,
  `Screen::alt_active()`, and a `PREDICTED` style flag.
- **`vttest`/`esctest` conformance vectors** (§16). The 50 tests here are
  hand-written; the real conformance corpus is not wired up.
- **Wide-character handling.** `WIDE_SPACER` is defined and documented but
  nothing yet computes character width, so CJK and emoji occupy one cell.
- **No PTY, no renderer.** This engine has never consumed real terminal output —
  only synthetic corpora. `parse_and_grid` at 47 MiB/s means one core can absorb
  47 MB/s, comfortably more than a PTY delivers, but the §4 row it serves
  ("`cat` 200 MB, sustained 60fps, zero dropped bytes") cannot be claimed until
  there is a renderer to drop frames from.

---

## Phase 5 — Predictive local echo (`CLAUDE.md` §5.5, §17 step 3)

Status: **complete.** 67 tests (up from 50), clippy `-D warnings` clean, fmt clean.

Done now rather than later on §17's own advice: *"Do this early — it shapes the
grid API, and retrofitting two-grid reconciliation later is painful."* The grid
API is a day old. It is also §1's largest single number — **20–40x** on perceived
keystroke latency — and unlike the transport or the renderer it can be verified
completely with no network and no display.

### Created — `crates/core-term/src/predict.rs`

The §5.5 loop, unchanged in semantics:

1. On keystroke, apply the predicted edit and render immediately; the bytes go to
   the server regardless — prediction never alters what is transmitted.
2. On server bytes, apply to the authoritative grid, then compare.
3. Match → confirm, drop the prediction.
4. Mismatch → **discard every outstanding prediction and snap**. §5.5: "Never try
   to partially repair; a hard snap is correct and cheap."

### The gate, which is the part that matters

§5.5: "Getting this wrong is worse than not predicting, so gate it hard." Every
listed condition is implemented and has a test:

| Refuses to predict when | Why |
|---|---|
| Alternate screen active | vim/htop assign arbitrary meaning to a keystroke |
| OSC/DCS string open | mid-sequence, the screen state is not what it appears |
| Bracketed paste in flight | the paste is not echoed character by character |
| Queue deeper than 12 | §5.5's cap on unconfirmed edits |
| Character would wrap the line | predicting a wrap means predicting a scroll — a wrong scroll flickers the whole screen instead of one cell |
| Key is not printable ASCII or Backspace | Enter, Tab, arrows, Ctrl-C are not predictable client-side |
| Backspace at column 0 | would require predicting a line join |
| **Server echo appears disabled** | **a password prompt. Painting typed characters there is a security problem, not a cosmetic one** |

Echo-off is inferred from consecutive silent timeouts and cleared the moment a
real echo is confirmed.

### Hit-rate instrumentation

§5.5: "If prediction hit rate is not instrumented, prediction is not shippable."
`Stats` tracks confirmed / missed / timed-out / flushes and exposes `hit_rate()`.
Below 85% the predictor **disables itself for the session** — but only after 20
samples, so one unlucky miss on a fresh connection cannot poison it. Both halves
are tested.

### The property `CLAUDE.md` §16 asks for, by name

> "proptest for the parser, reflow, **and prediction reconciliation** — property:
> for any input sequence, `predicted_grid` converges to `authoritative_grid`."

Three proptests, 400 cases each: convergence under arbitrary interleavings of
keystrokes and server output; a perfect echo always confirming with zero misses;
and the queue cap never exceeded for any input length.

### One test of mine was wrong, not the code

`a_bad_link_disables_prediction_for_the_session` failed on first run. The cause was
the test, not the predictor: it drove the server with `"\rZ"`, which left most
predictions *unresolved* rather than mismatched, so misses accrued too slowly to
reach the 20-sample threshold. Rewritten so each round is one unambiguous
mismatch.

### NOT done

- **Not wired to anything.** No PTY, no SSH transport, so this has only ever
  reconciled against synthetic server output. The 20–40x claim in §1 remains
  **unmeasured** — measuring it needs a real link with real RTT.
- `Kind::CursorOnly` is modelled and reconciled but nothing emits it yet; §5.5's
  "horizontal cursor movement" prediction needs arrow-key handling above this
  layer.
- The `PREDICTED` style flag exists and `underline_unconfirmed` is plumbed, but
  the renderer that would honour it does not exist.

---

## Phase 6 — A working terminal (PTY → engine → screen → keyboard)

Status: **complete.** The app runs a real shell. 4 session tests, 67 core-term
tests, clippy `-D warnings` clean across the workspace, frontend typecheck/lint/
build clean.

Before this, the engine had never seen a byte of real terminal output and the
window showed a placeholder. This closes the loop:

```
ConPTY → reader thread → Parser → Screen/Grid → generation counter
                                                      ↓  (UI samples per frame)
                                  dirty rows as text + style spans → canvas
keyboard → keyToBytes → session_write → PTY
```

### Created

| Path | Purpose |
|---|---|
| `crates/app/src/session.rs` | PTY spawn, reader thread, snapshot protocol, colour resolution |
| `crates/app/src/lib.rs` | `session_open/write/snapshot/resize/close` commands |
| `src/lib/session.ts` | typed IPC wrapper |
| `src/lib/keys.ts` | keyboard event → terminal bytes |
| `src/components/TerminalSurface.tsx` | canvas renderer, resize, focus, input |

### Verified end to end, not asserted

`session::tests::a_real_shell_echoes_into_the_grid` spawns a real `cmd.exe` over
ConPTY, writes `echo opspilot_marker`, and requires the marker to appear **twice**
in the grid — once as the echoed command line, once as the command's output. That
can only pass if a real shell ran and the parser handled its real output, which is
the first time any of this has been true.

Also tested: snapshots are incremental after the first (`full` only once), an idle
terminal returns no snapshot at all, resize forces a full repaint at the new
dimensions, and all 256 indexed colours resolve to valid CSS without panicking.

### Design points

- **The reader thread never pushes.** §13.1 rule 8 forbids one message per chunk.
  It bumps an `AtomicU64`; the UI asks once per animation frame. 10,000 PTY writes
  in a frame cost exactly one snapshot — §5.4's coalescing rule enforced at the
  IPC boundary.
- **Idle costs nothing.** `snapshot()` returns `None` when the generation has not
  moved, so a 60 Hz poll on an idle terminal is one atomic load and zero
  serialization.
- **Only dirty rows cross**, as text plus style spans rather than cells — a
  typical line is one or two spans against 200 per-cell objects.
- **`session_write` takes `Vec<u8>`**, not a string. Alt-modified keys and raw
  control sequences are not always valid UTF-8 and a `String` round-trip would
  corrupt them.
- **The slave PTY handle is dropped after spawn.** Without that the PTY never
  reports EOF when the child exits and the reader thread hangs forever on a dead
  session.
- **`keyToBytes` returns null for keys it does not own**, so the browser keeps its
  defaults and the shell's own `Ctrl+B`/`I`/`K`/`Enter` bindings still work.
- **Colours resolve in Rust**, so the webview never learns the palette. The ANSI 16
  are tuned to sit inside the chassis palette rather than glare against it.

### Debts, both deliberate and both recorded

- **D-19: the renderer is a 2D canvas**, which §3 ("anything updating faster than
  4 Hz is a wgpu surface") and Step 8 ("must NOT be an HTML element or xterm.js")
  both forbid. Taken knowingly so the app is usable before `crates/render` exists.
  The cost is exact: glyphs rasterise on the UI thread, so the §4 keystroke-latency
  and 200 MB `cat` rows **cannot be claimed from this renderer**. The engine
  measures 47 MiB/s; the ceiling is the canvas.
- **D-21: IPC is JSON**, which §3 also forbids. Mitigated by making the payload
  small enough that the encoding stops mattering. Disappears entirely once the
  terminal is a wgpu surface.

### NOT done

- **The predictor is not wired in.** Phase 5's `Predictor` needs to sit between
  keystroke and PTY write; bolting it on at the end of this phase would have been
  careless. On a local PTY it would also predict nothing useful — its value starts
  at real RTT.
- No tabs or splits: one session, opened on mount.
- No scrollback view. The ring is filling; nothing scrolls it yet.
- Mouse reporting, selection, and clipboard are absent.
- Incidental observation, not a budget claim: the previous build idled at **27.1 MB
  RSS** with no sessions. §4's row is "< 120 MB with 5 live sessions", which is a
  different measurement.

---

## Phase 7 — SSH and SFTP (`CLAUDE.md` §6, §8)

Status: **working and tested against a real SSH server.**

### The thing that made this verifiable

There is no sshd and no Docker on this machine, and "it compiles" is not evidence
that an SSH client works. `russh` ships a server, so
`crates/core-ssh/tests/integration.rs` stands one up on a loopback port — real
key exchange, real password auth, real channels, and a **disk-backed SFTP
subsystem**. That is stronger than a localhost sshd would have been: no external
state, and the test chooses the host key, which is the only way to deliberately
exercise a key rotation.

| Test | What it proves |
|---|---|
| `password_auth_opens_a_shell_and_echoes` | connect, auth, PTY dimensions reach the server, bidirectional data |
| `resize_reaches_the_server` | `window_change` arrives as `SIZE 120x50` |
| `a_wrong_password_is_rejected` | auth actually gates |
| `strict_policy_refuses_an_unknown_host` | Strict holds over a real handshake |
| **`a_changed_host_key_is_refused_over_a_real_connection`** | **the security boundary, end to end** |
| `sftp_lists_reads_and_writes` | list, read, write verified on disk, rename, delete |

### Two `CLAUDE.md` mandates that russh defaults against

- **`nodelay: true`** — §14.6: "TCP_NODELAY on every socket... one line and it is
  not optional." russh's `client::Config` defaults it to **false**.
- **8 MB channel window** — §6.1: "russh defaults are small (~2 MB or less); a
  narrow channel window stalls you no matter how deep the SFTP pipeline is."

### Host key verification

Its own module with its own tests, because it is the security boundary and
russh's `check_server_key` **defaults to rejecting everything** — a wrong
implementation is a silent downgrade, not a visible failure.

Three policies (Strict / AcceptNew / VerifyOnly), and one rule that does not
bend: **a changed key is refused under every policy**. Strict and VerifyOnly
provably never write to `known_hosts`; a key learned for `:22` does not authorise
`:2222`. The dialog has no "connect anyway" button, because a rotation and a
machine-in-the-middle are indistinguishable from the client.

### Transport pool (§8.1)

Keyed by the whole `(host, port, user, identity)` tuple. A shell and a file
browser on the same identity share one handshake; `SFTP·deploy` and
`SFTP·backup` deliberately do not.

### Two problems found on the way

1. **`aws-lc-sys` will not build here.** russh's default crypto backend compiles
   AWS-LC from C and needs NASM. Switched to the `ring` backend: pregenerated
   assembly, builds with a Rust toolchain and a linker. Both use hardware AES,
   which is what §6.4 cares about.
2. **A test failure that looked like a security finding.** `ssh-key`'s
   `PublicKey` equality **includes the comment field**, and
   `learn_known_hosts_path` round-trips through `to_openssh()` — so a fixture
   carrying an `ssh-keygen` comment compared unequal to its own recorded copy and
   reported *changed host key*. Real server keys carry no comment; the fixtures
   were unfaithful. Worth knowing, because hitting that in production would be
   alarming and misleading.

### NOT done

- **§6.1 AIMD pipelining and §6.2 tar-pipe.** Both belong to the transfer queue,
  which does not exist. Directory transfers are **disabled, not silently broken**.
- **§12.3 vault.** Passwords and passphrases travel as plain `String` and live in
  memory for the duration of a connect. §12.3 requires `zeroize`d, `mlock`ed
  buffers in a separate broker process. They are never logged, stored, or
  returned to the webview — but that is short of the spec, and it is the largest
  outstanding security gap.
- SSH agent auth, jump hosts, keyboard-interactive/MFA.

---

## Phase 8 — Config store, key management, port forwarding

Status: **core-store complete (22 tests); port forwarding written, SOCKS5
tested; UI pending.**

### `crates/core-store` — config as the source of truth (§12)

- **Atomic writes**: temp → `fsync` → rename. §12.2: "A crash mid-write must not
  cost the user their host list." Windows needs the target removed first, which
  is why the temp file is complete and fsynced before that happens.
- **No absolute paths, ever** (§12.2, "the single most common thing that breaks
  an exported config"). `~` and `${env:VAR}` round-trip, and a test asserts a
  stored key path contains no drive letter.
- **TOML, not YAML** — §12.2's reasoning: "YAML will silently coerce a port value
  of `no` into `false`."
- **`schema_version` refusal**: a workspace written by a newer binary is refused,
  not rewritten.
- **A workspace name cannot escape its directory** — it becomes a filename, and
  `../../evil` is sanitised.
- **The model cannot hold a secret.** `Identity` has `key_path` and
  `password_ref`, and no field a password could go in. A test greps the
  serialized output for `password =` and `passphrase` so adding one breaks a test
  rather than slipping through.

### Key management

`core-store::keys` discovers keys in `~/.ssh` and **never reads private key
material**. Algorithm, comment and encryption status all come from the `.pub`
file or the unencrypted header of the private container; whether a key needs a
passphrase is determined structurally, never by attempting decryption.

### Port forwarding (§8.1)

- **Local (`-L`)**: bind a port, open a `direct-tcpip` channel per connection.
- **Dynamic (`-D`)**: a SOCKS5 proxy — no-auth, CONNECT only, with IPv4, IPv6 and
  domain address types. BIND and UDP ASSOCIATE are **refused explicitly** rather
  than accepted and left to hang.
- Binds **loopback by default**: defaulting to `0.0.0.0` would expose a tunnel
  into production to the whole local network.
- `TCP_NODELAY` on the accepted socket (§14.6) — a tunnel usually carries an
  interactive protocol.
- Per-forward counters are `AtomicU64`, sampled by the UI (§13.1 rule 8).
- **Remote (`-R`) is not implemented** and is reported as unsupported rather than
  silently doing nothing.

### A test-isolation bug worth recording

Four store tests failed together with "0 hosts after save". Cause: `isolate()`
sets `OPSPILOT_CONFIG_DIR`, which is **process-global**, and cargo runs tests in
parallel threads — so they were pointing at each other's config root. Fixed with
a shared mutex around the env-var-dependent tests. The failure mode is worth
remembering: it looks like a storage bug and is a concurrency bug.

---

## Phase 9 — Session management UI, text sizing, honest telemetry

Status: **complete.** 21 app tests, 22 store tests, 6 SSH integration tests.

### Session tabs (Termius-style)

Multiple concurrent sessions with a real tab strip: middle-click closes, `Ctrl+W`
closes, `Ctrl+1–9` jumps, `Ctrl+Shift+[`/`]` cycles. A cyan dot marks a remote
session — cyan is link state and nothing else (§0.2) — and `⇅` marks SFTP.

Tabs hold **no terminal state**. The grid lives in Rust and is fetched by
generation, so switching tabs is a mount rather than a transfer, and a background
tab keeps receiving output whether or not anything is looking at it.

### Real hosts, from disk

The tree reads `~/.config/opspilot/workspaces/*.toml`. The demo fixtures now
appear **only** in browser mode and are never mixed with real hosts. `+` adds,
`⋮` on a row edits. Every identity becomes an `SFTP·name` chip, per §0.6's rule
that multi-identity is invisible unless the UI names it.

### Landing page

With no session open, saved hosts are shown as cards with one chip per identity —
`SSH·deploy`, `SFTP·backup` — each opening the connect dialog prefilled for that
identity. §0.6: "each chip is a click target that opens that connection type
directly... the whole product thesis made clickable."

### Text size (§0.8)

`Ctrl` `+` / `-` / `0`, plus `−`/`+` buttons in the terminal header. 8–32px.

Four things that make it actually work:
- **Line height scales with the font** at the original 1.36 ratio. A fixed line
  height makes large text overlap and small text float.
- **The PTY is resized.** Cell size changes rows and columns, and a shell not
  told about it keeps wrapping at the old width.
- **Handled before key encoding**, so `Ctrl+-` never reaches the shell as input.
- **Registered globally**, not only on the terminal — the point of a zoom
  shortcut is that it works when you cannot read the screen.

Persisted machine-local, not in the portable config: §12.1 puts display
preferences in the "DO NOT COPY" half, because a size that suits a laptop panel
is wrong on the desktop you sync to.

### The status rail was lying, and now is not

**Corrected.** Phase 2 shipped the rail with plausible constants — `3.2 ms`,
`12.4 MB/s`, `chacha20-poly1305`, `7 sessions` — as placeholders, and they were
never replaced. That is worse than showing nothing: fabricated numbers in a tool
used to manage production teach the user to distrust the entire rail. Reported by
the user, not found by me.

Every field is now measured:

| Readout | Source |
|---|---|
| sessions | count in the Rust session registry |
| hosts | live transports in the pool |
| in / out | `AtomicU64` counters in the PTY reader, the SSH channel reader, and each forward |
| cipher | the **negotiated** cipher, captured from russh's `kex_done` |
| connect | measured TCP connect + handshake time |

Two deliberate choices:
- **`—` where a thing cannot be measured**, with a tooltip saying why. No SSH
  connection means no cipher: "a local shell is not encrypted, it is a pipe."
- **The label says `connect`, not `rtt`.** It is the handshake round trip, not
  §4's live per-keystroke RTT. Calling it `rtt` would be the same lie in a new
  costume.

Totals rather than rates, because a rate needs a sampled delta and §14.6's
1-second EWMA, which does not exist yet.

### NOT done

- **§0.4's LinkStrip** — the 2px live RTT sparkline per tab. It needs a real
  per-session RTT probe; drawing it from the connect time would be decoration
  pretending to be telemetry.
- Drag-to-reorder tabs.
- Throughput **rates** (needs the EWMA).
- `Ctrl+K` command palette still opens nothing.

---

## Phase 10 — Command palette, inspector, wide chars, reflow

Status: **complete.** 86 core-term tests, 21 app tests, 13 core-ssh, 22 store.

### Command palette (guide Step 7)

`Ctrl+K`. Searches hosts, per-identity SFTP entries and actions, grouped with
condensed eyebrows. `>` switches to action mode, `⇥` completes, `↑↓⏎` navigate,
Esc closes and **restores focus to whatever had it**.

Ranking is **frecency, not pure fuzzy**, which §0.7 is explicit about. Decayed hit
count (two-week half-life) added to the fuzzy score, logarithmic so the tenth use
matters far less than the second. Mounted once and toggled, per "must open in
under 30ms... Do not mount on demand."

### Inspector

Replaces the placeholder. Live per-session byte counters, dimensions, local vs
remote, and whether a session ended — from a new `session_stats` command.

CPU and MEM from the §0.5 sketch are **deliberately absent**: getting them from a
remote host means running a command behind the user's back or shipping an agent.

### Throughput rates (§14.6)

The status rail now shows a rate smoothed with a 1-second EWMA — "a number
jittering between 94 and 108 MB/s reads as broken even when it isn't." Below
1 KB/s it reads `idle`; a negative delta (restart) restarts the average rather
than emitting a spike. Totals moved to the tooltip.

### Wide characters (§5.1)

`WIDE_SPACER` was defined from the start and **nothing ever computed a width**, so
every CJK character and emoji took one cell and lines drifted a column per
character. `width.rs` adds East Asian Wide/Fullwidth plus Emoji_Presentation
ranges, binary-searched, with no new dependency — `unicode-width` is ~40 KB of
tables for coverage a terminal does not need.

Two traps the tests pin down:
- **Halfwidth katakana.** U+FF01–FF60 are fullwidth (2 cells) but U+FF61–FF9F are
  halfwidth (1). They sit in the same block. My first table got `ｱ` wrong and the
  test caught it.
- **Dingbats are mixed.** `✅` U+2705 is emoji and takes two cells; `✓` U+2713 two
  codepoints away is a text check mark and takes one. Widening the block would
  misalign every TUI that uses `✓`, `✗` or `⚠`, so the emoji codepoints are listed
  individually.

Screen-level: a double-width glyph wraps whole rather than straddling the edge,
combining marks do not advance the cursor, and backspace steps over a spacer
instead of landing inside a glyph.

### Reflow on resize (§5.6)

**Narrowing the window used to truncate every visible line — the text was simply
gone.** Resize now gathers logical lines across a new per-row `wrapped` flag,
re-splits at the new width, and keeps the bottom of the screen.

- The `wrapped` flag is what makes this possible: an auto-wrapped row and a row
  ending in a newline are identical in the cell planes, and joining the wrong
  ones mangles the screen.
- Flags rotate with rows on scroll, or reflow joins the wrong lines after a
  second of output.
- Trailing blank rows below the cursor are dropped before keeping the last
  `rows`, or an almost-empty screen pushes its own content off the top.
- **Contract change**: `resize` previously "preserved top-left". It now keeps the
  bottom, because that is where the prompt and cursor are. The old test was
  rewritten rather than deleted.

Scrollback reflow (§5.6's lazy 1,000-row chunks) is **not** done; only the
viewport.

### A benchmark result I cannot vouch for

`parse_and_grid` measured 47 MiB/s after the ring change and 10–18 MiB/s now,
which looks like a serious regression. I could not confirm it:

- `parse_only` is **unchanged** (369 MiB/s, "No change in performance detected"),
  so the parser is not involved.
- Disabling the two additions gave 26 MiB/s.
- **Identical code measured 20.5, then 10.0, then 15.1, then 18.2 MiB/s** — up to
  2x run-to-run variance, after hours of continuous compilation on this machine.

The added work is one `rotate_left(1)` on a 50-element array per scroll and one
bounds-checked write per newline. That is microseconds across the whole corpus
and is not consistent with a 2-4x regression, so the most likely explanation is a
thermally throttled machine — but that is reasoning, not measurement. **Re-measure
on a quiet machine before trusting any of these numbers**, including the 47.

---

## Phase 11 — Splits, broadcast, remote forwards

Status: **complete.** 7 SSH integration tests (up from 6), 86 core-term.

### Terminal splits (guide Step 9)

Layout is a **binary tree of panes with split ratios**, as Step 9 specifies. A
leaf is a terminal; a branch divides its area. Every operation is a tree edit,
which is why closing a pane collapses its branch into the surviving sibling with
no special cases. Each tab owns a tree, so an unsplit tab is just a tree with one
leaf and there is no separate single-pane code path to keep in sync.

Dividers drag to resize, ratio clamped to 0.1–0.9 so a pane can never be
squeezed to nothing.

### Broadcast, and why it is built the way it is

Step 9: *"Typing the same command into four production servers by accident is the
worst thing this app could let a user do."* So:

- **2px amber border** on every participating pane. Plain focus gets a hairline,
  so the two states cannot be confused at a glance.
- **Persistent amber pill in the status rail**: `BROADCAST · N panes`.
- Off by default, **never persisted**.
- **Disarms itself whenever the pane set changes.** Splitting while armed would
  silently widen the blast radius to a pane the user never opted in.
- Targets come from *this tab's* pane tree, never "every open session" — a
  background tab must not be typed into.

### A keybinding that would have broken the terminal

Step 9 specifies `⌘D` / `⌘⇧D` for splits. On macOS `⌘` is distinct from Ctrl, so
that is free. **On Windows and Linux the same chord is `Ctrl+D`, which is EOF** —
how you exit a shell. Taking it would break the terminal to add a
window-management shortcut.

Splits are therefore `Alt+Shift+=` / `Alt+Shift+-` off macOS (the Windows
Terminal convention), and the guide's bindings on macOS.

While fixing that I found a second bug: I had used `preventDefault` alone in a
capture-phase listener, which does **not** stop the event reaching the terminal's
own handler. `Ctrl+D` would have split a pane *and* sent EOF. Every handled chord
now calls `stopPropagation` as well.

### Remote (`-R`) forwards

Previously refused with an explanation; now implemented and tested. The direction
is inverted from `-L`: the server listens, and each connection it accepts arrives
as a channel via `server_channel_open_forwarded_tcpip`, which is then joined to a
local TCP service.

The transport keeps a port → sender table so the handler can route each inbound
channel to the forward that asked for it; a channel for a port nobody is
listening on is **closed rather than leaked**.

`a_remote_forward_delivers_a_server_initiated_connection` proves it end to end:
the test server honours `tcpip_forward`, opens a channel back, and the bytes are
asserted to arrive at a real local `TcpListener`.

One honesty detail in the UI: for `-R` the "listening" column reads
`server:PORT`, because the port is on the *server*, not this machine. Showing a
local address there would misrepresent where the socket is.

### NOT done

- **LinkStrip (§0.4)**, still. It needs a real per-session RTT probe, and russh
  exposes no way to time a round trip. Drawing it from the connect time would be
  decoration pretending to be telemetry.
- Terminal splits render one canvas per pane, not Step 9's "one wgpu surface with
  scissor rects" — that shape needs the renderer that does not exist (D-19).
- Saved forwards have an `auto_start` flag that nothing acts on yet.

---

## Phase 12 — the vault (§12.3)

Until now every connect needed the password retyped, and `CLAUDE.md` §12.3 was
the largest outstanding security gap. It is the section headed *"get this right
or there is no recovery"*.

### The decision the section is really about

§12.3 names the way to get this wrong:

> "If you make DPAPI the root of trust instead, the vault becomes permanently
> unopenable on any other machine, with no recovery path."

So the root of trust is **the master password and nothing else**. There is no
call to Windows Credential Manager, DPAPI, or any keychain anywhere in
`crates/core-store/src/vault.rs`. Every input to key derivation — the Argon2
parameters and the salt — lives inside `vault.bin`.

The test that proves it is `a_vault_opens_on_a_different_machine`: it writes a
vault in one directory, copies **only the file** somewhere else, deletes the
original directory, and opens it with nothing but the bytes and the password. If
any machine-local input ever creeps into the KDF, that test fails.

### File format

```
magic     "OPSPILOT-VAULT\0"
version   u8 = 1
kdf       u8 = 1 (Argon2id)
m_cost / t_cost / p_cost   u32 LE each   (65536 / 3 / 4 — §12.3's numbers)
salt      16 bytes
verifier  sealed(known plaintext, aad = "verifier")
count     u32 LE
records   count x { id_len u16 | id | sealed(secret, aad = id) }
manifest  sealed(blake3(all ids), aad = "manifest")

sealed(x, aad) = nonce[12] | len u32 | AES-256-GCM(x, aad)
```

Three properties fall out of that shape, each with a test that fails without it:

| Property | Mechanism | Test |
|---|---|---|
| Header cannot be tampered with | It feeds the KDF, so a changed byte yields a key that cannot open the verifier | `an_implausible_memory_cost_is_refused_before_allocating` |
| Ciphertexts cannot be swapped between records | The record id is the GCM associated data | `a_ciphertext_cannot_be_moved_between_records` |
| Records cannot be deleted outside the app | A trailing manifest seals a hash of the id list | `deleting_a_record_outside_opspilot_is_detected` |

That middle one matters more than it sounds. Plain AES-GCM would happily decrypt
a ciphertext moved from `stage/root` onto `prod/root` — the vault would hand
back staging's password for a production connection, with no error anywhere.

Also handled: a 4 TiB `m_cost` is refused *before* the allocator is asked; a
newer `version` is refused rather than silently downgraded on the next save;
`create` will not overwrite an existing vault; every read goes through a
bounds-checked cursor, and `a_truncated_file_errors_rather_than_panicking` cuts
the file at eight different offsets to prove it.

### Secrets in memory

§12.3: *"Never a `String`, never logged, never in a panic message. Write manual
`Debug` impls printing `<redacted>`."*

`Secret` wraps `Zeroizing<Vec<u8>>`; `RootKey` zeroizes on drop. Both have manual
`Debug` impls. `Secret`'s does not even print the length — that would leak
password length into any log line. `debug_output_never_contains_a_secret`
formats the vault, a secret, and the root key and greps the result.

Only one record is ever decrypted at a time: the in-memory map holds
*ciphertext*, and `get` unseals a single entry on demand.

### The broker boundary

§12.3 wants the secrets broker in **a separate process**, so "a webview XSS
cannot read the vault". **That is not implemented.** What is implemented is the
same property enforced at the IPC boundary instead:

> **No Tauri command returns a secret.**

The registered commands are `vault_status`, `vault_create`, `vault_unlock`,
`vault_lock`, `vault_list`, `vault_set`, `vault_remove`,
`vault_change_password`, `vault_audit`. None returns plaintext. The only
function that decrypts, `app::vault::resolve`, is `pub(crate)` and reachable
only from the connect path.

Authentication works by *reference*: the webview sends `vault:prod/deploy`, and
`remote::connect` resolves it natively. The password never crosses IPC in either
direction after it is first stored.

`no_command_returns_a_secret` reads this file's own `generate_handler!` list and
fails the build's test run if `vault_get`/`vault_read`/`vault_reveal`/
`vault_export` ever appears in it.

The residual gap is real and worth stating plainly: a webview compromise can
still *list* record names, *overwrite* them, and *connect as the user*. It
cannot exfiltrate the passwords. A separate process would additionally contain a
compromise of the native side; that part is still missing.

### Audit log

Hash-chained per §12.3: `h[n] = blake3(h[n-1] || seq || action || id)`, in the
**state** dir (machine-local history is not something you copy to a new PC).
Unlocks, failed unlocks, reads, writes, removals and password changes are
recorded. Record ids are sanitised before being written, because a newline in an
id would otherwise forge log lines —
`a_newline_in_a_record_id_cannot_forge_a_log_line`.

What the chain does and does not buy is stated in the code and in the UI: it
makes tampering *visible*, not impossible. Rewriting one entry breaks
verification at that sequence number
(`editing_an_audit_entry_breaks_the_chain`); truncating the tail leaves a chain
that still verifies, which is the documented limit of a hash chain with no
external anchor. `dropping_the_last_audit_entry_is_detected_on_the_next_append`
asserts that limit rather than pretending it away, and the panel shows
`broken at N` in red when verification fails.

### Wiring

- **`ConnectDialog`** — when the saved host carries a `password_ref`, the
  password field is *replaced* by the reference rather than shown empty (an
  empty field beside "using vault:prod/deploy" invites typing something that
  would be ignored). A locked vault gets an inline unlock, and Connect is
  disabled until it succeeds. Ticking "save to the vault" stores the credential
  **before** connecting — saving afterwards would lose it on any unrelated
  connect failure, which is the exact moment the user most wants it kept.
- **`HostEditor`** — a secret picker per identity, listing ids only. Writes
  `password_ref = "vault:<id>"` into the workspace TOML.
- **`VaultPanel`** — create / unlock / list / add / remove / change password /
  audit log. It states outright that secrets cannot be shown, because the
  absence of a reveal button otherwise reads as a missing feature.
- **`StatusRail`** — a `vault N` / `vault locked` chip, polled at 5s because the
  backend auto-locks on an idle timer and a one-shot read would go stale.
- **Command palette** — "Vault".

Auto-lock is 15 minutes idle. An unlocked vault is a decrypted root key in RAM.

### Verified

- **46 core-store tests** (was 22) — 24 new, all vault or audit log.
- **8 core-ssh integration tests** (was 7). The new one,
  `a_secret_stored_in_the_vault_authenticates_a_real_connection`, is the only
  test that proves the feature actually works: it stores the server's password,
  drops the vault, reopens it from disk, resolves the reference, and completes a
  **real SSH handshake** against the in-process russh server — then lists a
  directory over SFTP to show the session is usable, not merely authenticated.
  A byte out of place anywhere in Argon2 or AES-GCM fails it.
- **28 app tests**, including `no_command_returns_a_secret`,
  `a_vault_reference_against_a_locked_vault_fails_the_connect`, and
  `create_store_lock_unlock_resolve` — which drives the command layer itself
  (create → set → resolve → lock → wrong password → unlock → remove) under the
  shipped 64 MiB Argon2 parameters, and then asserts every one of those actions
  landed in the audit log with the chain still verifying.
- Workspace total **183 tests**, `cargo clippy --workspace --all-targets` clean
  (the pre-existing lints in `keys.rs`, `paths.rs` and `store.rs` were cleared
  too), `cargo fmt` applied, typecheck / eslint / vite build clean.

### Fixed on the way

`forwards::start` looked the connection up **before** validating the request, so
a remote forward missing its target reported `not connected` — sending the user
to debug the wrong thing. Argument validation now happens first. Found because
renaming the stale `remote_forwards_are_refused_with_a_reason` test (remote
forwards work now) made it assert the error *names what is missing*, and it
didn't.

### NOT done

- **`mlock` / `VirtualLock`.** Memory is zeroized but not locked, so a derived
  key can in principle reach the page file. Needs platform bindings this crate
  does not have.
- **Separate broker process** — see above.
- **OS-keychain convenience wrap.** §12.3 permits storing a *wrapped copy* of
  the derived key for biometric unlock, explicitly as a convenience and never as
  the root of trust. Not implemented; the master password is typed every unlock.
  This is the safe direction to be incomplete in.
- **Key-file passphrases** have a `passphrase_ref` field wired end to end, but no
  UI writes one yet — only `password_ref` is exposed in `HostEditor`.

---

## Phase 13 — Linux

Everything before this was built and run on Windows. The Rust already had
`#[cfg(unix)]` arms (agent socket, mode bits, PTY via `openpty`), but nothing
had been looked at from a Linux desktop. What was wrong, by consequence:

| Fix | Where | What happened without it |
|---|---|---|
| `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=OpsPilot`; strip `KITTY_*`/`VTE_VERSION`/… | `app/src/session.rs` | Launched from a desktop icon the shell got **no `TERM`** — no colour, `clear` fails, vim degrades. Launched from kitty it inherited `xterm-kitty` and emitted sequences for a terminal this is not. Windows never noticed: ConPTY ignores `TERM`. |
| PTY read retries `EINTR` | `app/src/session.rs` | `Err(_) => break` treated a signal landing mid-read as the session ending. |
| Shell list from `/etc/shells`, deduped by canonical path, `nologin`/`tmux`/`rbash` excluded | `app/src/shells.rs` | Hardcoded `/bin/zsh` beside `$SHELL=/usr/bin/zsh` offered **zsh twice** on every usr-merged distro; `/usr/bin/fish` missed fish elsewhere. |
| State dir is `$XDG_STATE_HOME` (`~/.local/state/opspilot`) | `core-store/src/paths.rs` | Was `~/.local/share`, which XDG reserves for user data. Windows/macOS unchanged. |
| `portable()` matches `$HOME` on a component boundary | `core-store/src/paths.rs` | With `$HOME=/home/al`, `/home/alice/key` was stored as `~ice/key`. All platforms; found here. |
| `portable()` rewrites `\` only on Windows | `core-store/src/paths.rs` | On Unix `\` is a filename character; rewriting it points at a different file. |
| Atomic write fsyncs the parent directory on Unix | `core-store/src/paths.rs` | ext4/XFS can roll back a rename after power loss until the directory is synced. §12.2's "a crash mid-write must not cost the user their host list" was only half true. |
| Local pane follows symlinks | `app/src/localfs.rs` | `/bin`, `/lib` and dotfile-managed `~/.config` entries showed as `symlink` and could not be opened. Dangling links still show as `symlink`. |
| `WEBKIT_DISABLE_DMABUF_RENDERER=1` **only** when `/proc/driver/nvidia` exists | `src-tauri/src/main.rs` | Proprietary NVIDIA + WebKitGTK gives a blank window (X11) or `Error 71` (Wayland). Not applied on Mesa, where DMA-BUF is the faster path. |
| `.deb` section, desktop category, hi-DPI icon | `tauri.conf.json` | `.desktop` file had no category, so launchers filed it under "Other". |
| `npm run doctor` checks `cc`, `pkg-config`, WebKitGTK 4.1 / libsoup3 / GTK3 via pkg-config, with the install command for apt/dnf/pacman/zypper | `scripts/doctor.mjs` | `webkit2gtk-sys` panics in its build script with a pkg-config error that does not name the package to install. |

### Verified

On Kali rolling (Debian family, X11, no NVIDIA), rustc 1.100.0, WebKitGTK 2.52:

- `cargo test --workspace` — **214 passed, 0 failed**, including the new
  `a_sibling_of_home_is_not_collapsed_to_tilde`,
  `backslash_is_a_filename_character_on_unix`, `no_binary_is_offered_twice`
  and `non_interactive_entries_are_never_offered`. The first two would have
  failed against the old `portable()`.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean, after one
  fix: clippy 1.100 adds `manual_slice_fill`, which flagged the two dirty-bitset
  loops in `core-term/src/grid.rs`. Now `.fill()`; same codegen, 101 core-term
  tests still pass.
- `npm run build`, `npm run lint` — clean.
- `npx tauri build` — release binary (8.6 MB, fat LTO) plus `.deb`, `.rpm` and
  AppImage. The `.deb` carries `Section: net`,
  `Depends: libwebkit2gtk-4.1-0, libgtk-3-0`, and `Categories=Development;`.
- The release binary launches: a 1600×1000 `OpsPilot` window appears under
  X11, and it runs with nothing on stderr.
- `npm run doctor` detects the distro and prints `apt` commands.

**Not exercised:** a terminal session inside the window, the shell picker, the
local pane following symlinks, Wayland, and the NVIDIA workaround (this machine
has no NVIDIA driver, so that code path did not run). Those need a person at the
window.

Setup gotcha: on a rolling distro, `apt install` 404s until `apt update`
refreshes a stale package index. And `sudo` cannot prompt from a TTY-less
shell, so the install has to be run in a real terminal.

### NOT done

- No Flatpak/Snap manifest. `.deb`, `.rpm` and AppImage come from `tauri build`.
- The two items that were listed here (no clipboard; Ctrl+B/K stolen from the
  shell) are done in Phase 14.

---

## Phase 14 — Linux: the input side

Phase 13 made the app start correctly on Linux. This phase makes it usable as a
terminal there. Every item below is something a Linux user would hit within
minutes. Most were also wrong on Windows but **hidden by ConPTY**, which tracks
terminal modes and re-encodes input itself. A raw Unix PTY does none of that.

| Fix | Where | What happened without it |
|---|---|---|
| **App chords are Ctrl+Shift off macOS** (D-24) | `lib/platform.ts`, `AppShell`, `SessionTabs` | **Ctrl+W closed the session** mid-command instead of deleting a word. Ctrl+K (kill-line), Ctrl+B (tmux prefix) and Ctrl+I (= Tab) never reached the shell. |
| **DECCKM tracked and honoured** | `core-term/screen.rs`, `lib/keys.ts` | Arrows always sent `ESC[A`. ncurses (htop, nano, mc, tig) enables application-cursor mode via terminfo `smkx` and then only recognises `ESC OA`. **zsh sets it at every prompt** (found by a test that assumed it would be off). |
| DECSET parameter lists | `screen.rs` | `ESC[?1;1049h` applied only the first mode. |
| RIS restores input modes | `screen.rs` | `reset` after a crashed TUI left arrows in application mode at the prompt. |
| Cursor hidden on `?25l` | `screen.rs`, `session.rs`, `TerminalSurface` | An amber block sat on whichever cell vim/htop last drew. Also hidden while scrolled back. |
| **Selection** — drag, double-click word, triple-click line, Shift+click extend | `TerminalSurface` | No way to select terminal text at all. Drawn on a separate overlay canvas, so damage repaints never erase it. |
| **Copy/paste** — Ctrl+Shift+C/V, Ctrl/Shift+Insert, select→PRIMARY, middle-click pastes PRIMARY | `app/src/clipboard.rs`, `lib/session.ts` | No clipboard on any platform. Linux uses the window's GTK clipboard (X11 + native Wayland). See D-25. |
| **Bracketed paste**, with embedded `ESC[201~` stripped | `session.rs::paste_bytes` | Pasted text containing a newline would execute. Hostile clipboard text could close the bracket early. |
| Paste line endings → CR | `paste_bytes` | CRLF from a Windows-sourced clipboard pressed Enter twice. |
| xterm modifier encoding: Ctrl+←/→ = `ESC[1;5D/C`, Shift+Tab = `ESC[Z`, Alt+Backspace, Ctrl+Backspace, modified F-keys | `keys.ts` | Ctrl+←/→ (word jump) sent a plain arrow. Shift+Tab sent Tab. |
| AltGr characters typed | `keys.ts` | Windows reports AltGr as Ctrl+Alt, so `@` on a German layout was **dropped**. |
| Ctrl+letter by physical key on non-Latin layouts | `keys.ts`, `platform.ts` | Ctrl+C did nothing on a Cyrillic layout. |
| IME composition ignored | `keys.ts` | Keys mid-composition were sent to the shell. |
| Font zoom no longer steals Ctrl+_ (readline undo) | `state/settings.ts` | |
| Font zoom changed size **twice** per press in a terminal | `AppShell` | Both the window and terminal handlers acted on it. |
| Esc in a zoomed terminal no longer unzooms | `AppShell` | Every Esc in vim kicked the pane out of zoom. |
| Tab cycling: Ctrl+PageUp/Down; ⌘⇧[ ] matched by `code` | `SessionTabs` | Shift+[ arrives as `{` on most layouts, so the old check never fired. |
| **New tabs closed the instant they opened** | `PaneTree.tsx` | On first render `tree` is `undefined` (`ensure` has not run), and the close-when-empty effect fired in the same commit with that stale value. **No terminal could be started at all.** Pre-existing in the committed code; Phase 13 never opened a session. Now closes only once a tree has existed. |
| `bench:filter` and the new `test:keys` run through esbuild | `package.json` | Debian/Kali's `nodejs` is built **without TypeScript stripping** (`process.features.typescript === false`), so `node script.ts` fails. |

### Verified

- `cargo test --workspace`: **227 passed**, 0 failed (was 214). New tests:
  - four for modes in `core-term`, including DECSET lists, ANSI-vs-private, and RIS;
  - `paste_bytes`: line endings, wrapping, the bracket-escape attack, partial markers;
  - two **through a real PTY**. `modes_set_by_the_program_reach_the_snapshot` runs
    `sh -c` with `printf`. `a_paste_reaches_the_program_bracketed` puts the PTY in raw
    mode and reads back the exact bytes with `od`: start marker, one CR for a CRLF,
    end marker;
  - three for `clipboard`, plus `no_command_returns_a_secret` extended to ban
    `clipboard_read`.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `npm run test:keys`: 8 passed. Covers DECCKM, modifier encoding, the formerly
  stolen Ctrl+letters, the Cyrillic layout, AltGr, IME, and the chord rules.
- `npm run typecheck`, `npm run lint`, `npm run build`: clean.
- **In the real app**, debug build in a nested Xephyr display (`:99`). Isolated:
  synthetic input and its clipboard cannot reach the desktop. Each item was checked
  by a file the shell wrote, by `xclip -o`, or by a screenshot:
  - `Ctrl+W` deletes a word and the session survives. `Ctrl+K`, `Ctrl+B`,
    `Ctrl+←` and `Ctrl+I` (Tab completion) all edit the line as a shell expects.
  - With `?1h` set, Up arrives as `033 O A`; without it, as `033 [ A`.
  - `Ctrl+Shift+V` pastes. A pasted **two-line command did not run** until Enter
    (bracketed paste), then both lines ran.
  - Double-click puts the word on PRIMARY. `Ctrl+Shift+C` copies to CLIPBOARD,
    which is not touched before that. Middle-click pastes PRIMARY. Triple-click
    takes the row, and a drag spans rows.
  - `?25l` hides the cursor (0 cursor pixels) and `?25h` restores it.
  - One `Ctrl+=` steps the font once (12.5 → 13.5). `Ctrl+Shift+E` toggles the
    tree. `Ctrl+Shift+W` closes the session.
  - Box-drawing in the zsh prompt renders. On Linux, canvas falls back per glyph
    to the system monospace, so Pending 5.1's tofu does not show here.
- Xephyr has no DRI3, so it needs `WEBKIT_DISABLE_DMABUF_RENDERER=1`, and with no
  window manager mouse clicks need the window focused first. Neither applies on a
  normal desktop.

---

## Phase 15 — Docker, Kubernetes, Databases

Built on request ahead of §17's order; see D-26. Every module is live and verified
against a real service, not a mock.

### Shared pieces

| Piece | Where | Notes |
|---|---|---|
| **Stream session backend** | `app/src/session.rs` (`StreamIo`, `spawn_stream`) | Container and pod exec/logs are ordinary terminal sessions, fed by plain tokio channels (D-27). Logs get LF→CRLF, since there is no PTY to do `onlcr`. Read-only streams drop keystrokes. Closing the tab closes the producer. |
| **`DataTable`** | `src/components/DataTable.tsx` | Rows are pulled by index (`rowAt`), so a million-row result can stay in Rust. Virtualised, keyed rows, sticky header, resizable columns, keyboard navigation. Sort is the caller's job. |
| Tab `source` | `lib/streams.ts`, `state/sessions.ts` | A tab can be "exec into X" or "logs of Y". Splitting that pane opens a second exec. |
| `ModuleKit` | `src/components/ModuleKit.tsx` | Header, status dot, two-step confirm for destructive actions, filter box, generation poll hook. |

### Docker — `crates/core-docker`, `app/src/docker.rs`, `DockerPanel.tsx`

- **Endpoint resolved like the docker CLI:** `DOCKER_HOST`, then `DOCKER_CONTEXT`
  or `currentContext`, then the default socket. bollard's own default only looks
  at `/var/run/docker.sock`, which **does not exist under Docker Desktop for Linux**.
  The development machine is that setup, so the obvious implementation would have
  reported "Docker not running" with Docker running.
- **Live list from the events API, never polled (§10).** A burst of events
  (`compose up`) is coalesced into one re-list. CPU and memory come from one stats
  stream per running container, using `docker stats` arithmetic (page cache
  excluded). Reconnects use exponential backoff with jitter (§8.3).
- Actions: start, stop, restart, pause/unpause, remove (two-step confirm). Exec
  shell (bash, falling back to sh) and logs follow, both as terminal tabs.
- Container ids are validated against Docker's name grammar before any request.
  bollard puts the id straight into the URL path, so `../` would reach another API
  endpoint.
- Commands that wait on the daemon are `async`. Tauri runs sync commands on the
  main thread, and `docker stop` waits out a 10 s grace period.

### Databases — `crates/core-db`, `core-store/src/databases.rs`, `app/src/db.rs`, `DatabasesPanel.tsx`

- **Postgres** (tokio-postgres): binary protocol, and a real cursor (portal inside
  a transaction). Values are decoded by OID without chrono/uuid/decimal crates:
  numeric, date, timestamp(tz), time, interval, uuid, json(b), inet/cidr, bytea,
  1-D arrays, enums, domains.
- **MySQL/MariaDB** (mysql_async, pure-Rust deflate). The stream pauses between
  chunks under TCP backpressure. Cancel sends `KILL QUERY` over a second connection.
- **SQLite** (rusqlite, bundled) runs on its own thread. A file is never created
  if missing. Scripts run statement by statement; see below.
- **Results never leave Rust.** Rows arrive in 500-row chunks into typed Arrow
  columns. The engine parks until the UI scrolls near the end. Sort and filter
  run in Rust over the whole result (arrow kernels, not DataFusion: D-28). Only
  the painted window is formatted and sent. A sort or filter switches the cursor
  to 20k-row fetches, so a bulk load over WAN costs tens of round trips, not
  hundreds.
- Saved profiles live in `databases.toml`. **The model has no password field**:
  passwords are vault references or typed per connect. SQLite paths are stored as
  `~/…` (§12.2).
- A new query cancels the running one. The session is one connection, so `SET`
  and temp tables persist between queries.
- No TLS yet (D-30). The panel warns when a profile points at a non-loopback host.

Bugs the tests caught:
- **rusqlite's `prepare` silently drops everything after the first statement.**
  "create; insert;" reported success having only created the table.
- Counting statements first doesn't work either: preparing statement 2 fails
  before statement 1 has created its table. Scripts now run statement by statement.
- The first sort reported **18 400 ms**, which was mostly the cursor waiting for
  me to scroll. Reported time now excludes parked time.

### Kubernetes — `crates/core-kube`, `app/src/k8s.rs`, `KubernetesPanel.tsx`

- Reads kubeconfig the way kubectl does (`$KUBECONFIG`, then `~/.kube/config`), so
  contexts and exec auth plugins work.
- **Informer per (context, kind, namespace)**, using the watcher's WatchList
  bootstrap and falling back to List+Watch on the first failure. The store is
  keyed by UID and a relist swaps in atomically. An informer nobody samples for
  60 s is dropped, closing its watch.
- Pods, Deployments, Services, Nodes, plus Namespaces for the picker. The STATUS
  column follows **kubectl's rules**: `CrashLoopBackOff`, not the phase "Running".
- Pod exec (sh, `TERM` exported, resize forwarded), logs follow with a container
  picker, delete pod, deployment scale ± and rollout restart, YAML view without
  `managedFields`.
- JSON, not protobuf (D-29). TLS is rustls with the `ring` provider, per §18.7.

### Verified

- `cargo test --workspace`: **267 passed**, 0 failed (was 227).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `npm run typecheck`, `lint`, `build`, `test:keys`: clean.
- **Live suites**, all passing. Run with `--ignored`; setup is in each file's header.
  - `core-docker/tests/live.rs`: against Docker Desktop through the `desktop-linux`
    context. It creates a labelled container, sees it through events, gets stats,
    execs (sh fallback on alpine), follows logs, stops it, removes it, and checks
    that an invalid id is refused.
  - `core-db/tests/live.rs`: against Postgres 16 and MySQL 8.4 containers. It
    checks:
    - the cursor parks after one chunk;
    - sort and filter in Rust;
    - DML affected counts and server error text;
    - a 30 s `sleep` is cancelled by the next query;
    - 10 Postgres types decode exactly.
  - `core-kube/tests/live.rs`: against a kind cluster (k8s 1.34). The informer ran
    in **watchlist** mode. It covers pod to Running, logs, exec with
    `TERM=xterm-256color`, scale to 2/2, rollout restart, YAML, and a deleted pod
    leaving the store.
- **Timings** (release build, 200k-row Postgres table, local container):

  | | |
  |---|---|
  | first 500 rows | 4.3 ms |
  | load all + sort | 366 ms |
  | sort only | 2.1 ms |
  | substring filter | 36 ms |

- **In the real app** (nested Xephyr display, scratch config and kubeconfig):
  - Docker: your 6 containers listed with live CPU/memory. A container started
    from the CLI appeared without a refresh. Exec, logs, stop and remove all worked
    from the panel.
  - Databases: connect with a typed (unsaved) password. 200k rows stream as you
    scroll. Sorting by amount loaded and sorted everything.
  - Kubernetes: the list showed "watching · watchlist". Pod exec, scaling to 3/3
    (confirmed with the cluster's own kubectl) and the YAML view all worked.
- **Binary size: 8.6 MB → 22.9 MB** (release, fat LTO, stripped), measured with
  `cargo build --release -p opspilot-tauri`. This is the largest cost of the phase.
  The contributors are kube + k8s-openapi (generated API types), arrow, bundled
  SQLite, mysql_async and bollard. A per-crate breakdown needs `cargo bloat`, which
  is not installed here. §3 wants engine crates behind feature flags; they are not
  yet, so a build without Kubernetes is not possible today. That is the obvious
  first lever if size matters.

### NOT done

- REST/API client and log viewer modules (still placeholders).
- Database TLS (D-30), CSV export, schema browser and autocomplete, query history.
- Docker: images, volumes, networks, and `ssh://` endpoints.
- Kubernetes: protobuf (D-29), other kinds (StatefulSets, Jobs, ConfigMaps,
  Ingress…), events, port-forward, multi-container log merge.
- The inspector labels exec and log sessions "local #N".

---

## Phase 16 — Leak and robustness audit

Three independent reviewers read the code cold: the Rust session/Docker/Kubernetes
layer, the database stack, and the frontend lifecycles. Each finding acted on was
verified against the code first, and against the library source wherever the
behaviour depended on it (bollard, kube-client, tokio-postgres, mysql_async,
rusqlite, portable-pty, tokio). Most of the high-severity findings predate this
session.

### High severity — fixed

| Finding | Fix |
|---|---|
| **Switching tab or module, or splitting a pane, killed the running shell** and opened a new one. The terminal component owned the session, and React unmounts components for layout reasons. | Sessions belong to **panes** (`state/terminals.ts`). A mount reattaches; only closing the pane or tab ends the session. Snapshot `since == 0` is always a full repaint. |
| **Every SSH, exec or log pane opened two sessions and leaked one.** Shell discovery changed `shellId`, which re-ran the open. | Shells are only discovered for local panes. The open is keyed on what is opened. |
| **An open still in flight when its pane closed leaked forever.** | The pane store closes a late open the moment it resolves. |
| **Postgres: a SELECT committed or rolled back the user's own transaction** (`BEGIN; DELETE …;` then a check). | Probe with a savepoint. Inside a user transaction, the cursor runs in `SAVEPOINT` and the outer handle is never dropped (its Drop sends ROLLBACK). |
| **A parked cursor held its transaction (Postgres) or metadata lock (MySQL) indefinitely**, blocking VACUUM and `ALTER TABLE`. | Parked cursors close after 60 s idle (`PARK_LIMIT`). Fetched rows stay on screen and the status says so. |
| **Kubernetes/Docker exec deadlocked on a paste.** One loop wrote stdin and read stdout; kube's attach buffers are 1 KiB. Closing the tab did not free it. | Separate reader and writer tasks; the reader aborts the writer. kube buffers raised to 64 KiB. Live test: 1 MB paste with echo through a Docker exec. |
| **A webview reload leaked every session**: shells, SSH channels, DB connections and their transactions, watches. | `app_reset`, awaited before first render. Verified by reloading the real app: both shells and the Postgres connection were released. |
| **Sync commands did network, disk and KDF work on the UI thread.** An SFTP transfer froze the window for its full length; an SSH connect to a dead host for ~2 min; vault unlock for the KDF. | 35 commands moved to the blocking pool (`blocking()` in `lib.rs`). Only in-memory commands remain sync. |
| **DB sort/filter could freeze the UI thread** (a mutex held through a 2 M-row sort; `cancel` building the view). | The view is built outside the lock from a snapshot and installed only if still current. `cancel` reads atomics. DB commands are async. |
| **No byte limit on a result; one large chunk could abort the app** (an Arrow string offset over 2 GiB panics, and release is `panic = "abort"`). | 512 MB budget. Chunks flush at 64 MB. Cell display is truncated at 16 KB; export is not. |
| **The PTY writer blocked the UI thread** when the program stopped reading stdin. | Writer thread behind a bounded channel; a full queue is refused with an error. Test: 8 MB into a program that reads nothing returns an error and does not hang. |

### Medium and low — fixed

- **Terminal sessions:**
  - A PTY reader leaked (thread plus screen) when a detached job held the PTY slave. It now polls with a timeout. Tested with `setsid sleep`.
  - Close SIGHUPs the foreground process group.
  - The child is reaped without holding the lock, and never signalled once reaped, so a recycled PID can't receive it.
  - Spawn error paths kill the child.
- **Docker:**
  - The watch is dropped after 60 s unsampled. It used to run one stats stream per container forever.
  - Events are lifecycle only; healthcheck execs caused constant re-lists.
  - Reconnect backoff resets after a successful connection.
  - The endpoint is cached while the daemon is down.
- **Kubernetes:**
  - Informers are swept on a timer, not only while the panel is visible.
  - Non-watch calls time out after 30 s; kube's default read timeout is none.
- **Database:**
  - Cancel could be lost (missed-wakeup race).
  - A superseded query is never executed.
  - A server-side cancel is sent only if something is actually executing; a parked cancel used to kill the *next* query (live-tested on both engines).
  - MySQL: a bounded drain plus `KILL QUERY` after an early stop. An abandoned connection is closed off-runtime, because on-runtime mysql_async drains the pending result forever.
  - SQLite: an interrupt handle (Cancel stops a running statement), `busy_timeout`, and a bounded job queue.
  - Postgres: a connect timeout covering startup and auth, plus keepalives. Infinite intervals no longer overflow, and a hostile array header no longer triggers a 50 GB allocation.
  - The data mutex is no longer locked once per row.
  - A dropped Sink marks its query done.
  - Profile file I/O is off the UI thread.
- **Frontend:**
  - A Kubernetes context switch kept the old cluster's rows on screen, so actions could hit the wrong cluster. Poll data now resets on source change.
  - Confirm buttons are bound to their target; armed on A could remove B.
  - DB connect: double-open, unmount and reconnect races; out-of-order run results.
  - The idle DB poll re-rendered 5×/s. The k8s list re-sorted every second for the age column.
  - Column-resize listeners leaked, and a resize toggled the sort.
  - Split drags stuck after Alt-Tab.
  - Unhandled rejections from keystrokes into exited sessions.
  - Module panels stay mounted, so leaving Databases no longer drops the connection. Hidden panels pause polling, so the backend's idle sweep releases their watches.
- Clipboard reads time out after 5 s; pastes over 32 MiB are refused.

### Verified

- `cargo test --workspace`: **276 passed**, 0 failed. New regression tests:
  - writes never block;
  - the reader exits with a detached job holding the PTY;
  - `close_all`;
  - lost-cancel race;
  - idle park closes (virtual time);
  - byte budget and chunking;
  - dropped Sink;
  - infinite interval and hostile array;
  - paste cap.
- Clippy `-D warnings`, typecheck, lint, build, `test:keys`: clean.
- **Live:**
  - DB suite (6). New tests: a user transaction survives a SELECT and a failed SELECT; a parked cancel spares the next query on Postgres and on MySQL.
  - Docker suite (2), with the 1 MB paste.
  - A benchmark test that hung forever on a missing table now has deadlines.
- **Real app** (nested display; each result read from shell-written files and server state, not pixels):
  - The same shell PID survived a module round-trip and a split, and the original pane kept its history.
  - A background tab kept running.
  - Closing a tab killed both of its shells.
  - The DB connection survived module switches (same server backend pid).
  - A webview reload released all shells and the Postgres connection.

### Not fixed — known

All four items are closed by Phase 17, except live-testing the Kubernetes exec.

---

## Phase 17 — Closing the Phase 16 leftovers

### Fixed

| Item | Fix |
|---|---|
| **SSH shell channels were unbounded** (§13.1 rule 7). A server that stopped reading let typed or pasted input queue without limit. | Bounded input (256 batches) and output (64 packets, at most 4 MiB). Input is **refused**, not queued, once the remote stops reading. Resize is a watch (latest wins, never dropped). Reader and writer are separate tasks, the same paste-deadlock fix as the Docker/K8s execs. Close stops both tasks even mid-write against a stalled server. The remote-forward queue is bounded too (a 64-connection backlog, past which connections are refused). |
| **SSH connect had no timeout of its own.** A host that drops SYNs took ~2 min; one that accepts TCP but never speaks SSH waited forever. | `Timeouts { connect: 20 s, auth: 90 s }`. Auth is longer because it can wait on a person (a hardware-key touch). `Transport::connect_with` takes custom values. |
| **Exec'd processes outlived their tab.** Closing a Docker or Kubernetes exec does not hang anything up, so `top` and `tail -f` kept running in the container. | The exec wrapper exports `OPSPILOT_SESSION=<token>.$$`. When the user closes the tab, one more exec runs `exec_hangup.sh`. It reads the shell's pid back from any process carrying the token and SIGHUPs that **one session**, which is what a terminal hangup does. As with a local terminal, `nohup` survives, a `setsid` daemon survives, and a scrubbed environment (`sudo`) is still caught. It never signals its own session or PID 1's. Docker skips it if the exec has already ended. |
| **The Kubernetes client cache was never invalidated on 401.** After a token revocation, cert rotation or re-login, every call kept failing with the stale client. | Credential failures (API 401, 401 on the exec websocket upgrade, a failed exec auth plugin) produce one recognisable error. Actions, YAML, exec and logs drop the cached client on it. A watch failing on credentials gets its client and informer rebuilt, at most once per 15 s. 403 is **not** treated as a credential failure, because RBAC denials are not fixed by re-authenticating. |

### Verified

- `cargo test --workspace`: **284 passed**, 0 failed (was 276). New tests:
  - `core-ssh/tests/integration.rs`: a host that accepts TCP and never speaks fails with "no SSH answer" instead of hanging. A stalled server gets input refused within bounds, and close still ends the output stream.
  - `core-docker/tests/hangup.rs`: the hangup script against real processes on this machine, run under **both dash and busybox ash**. It hangs up the leader, a background job and an env-scrubbed job. A `setsid` daemon, another tab's session and an unrelated process survive. An unknown token signals nothing; a missing token exits 2.
  - core-kube: 401 versus 403/404/409/500, the upgrade 401, all watcher error variants, and the wrapper/token format.
- The first version of the script also killed `setsid` daemons, because they inherit the token. The test caught it, and the script now targets the recorded session only.
- Clippy `-D warnings`: clean.

### Not done

- **Live runs.** Docker Desktop was not running and `kind`/`kubectl` are not installed, so neither the Docker nor the Kubernetes live suite ran this round. The hangup is proven against real processes but not yet inside a real container. The Phase 16 Kubernetes exec split is still not live-tested.
- The hangup needs `sh`, `tr` and `cat` in the image. Distroless images have no shell to exec into in the first place.

---

## Phase 18 — Predictive echo on a real link, and SSH agent auth

### Agent auth

`core-ssh` could already authenticate through an agent, but the app never
offered it: `remote.rs` built only a key-file method and a password method.

- Order is now **configured key → agent → password**. A configured key goes
  first because each rejected agent key counts against the server's
  `MaxAuthTries`.
- `Identity::use_agent` defaults to on, as with `ssh`, and is written to TOML
  only when off, so existing workspace files do not change.
- The connect dialog shows whether an agent is running and how many keys it
  holds (`ssh_agent_status`: public keys and comments only). The host editor
  has a per-identity toggle.
- Agent discovery is one function shared by auth and status (`agent::connect`,
  type-erased with `AgentClient::dynamic`). The Windows branch is unchanged
  apart from `.dynamic()`; no Windows target is installed here, so it was not
  compiled.

### Predictive echo: wired, then fixed

The predictor was already in the SSH session path (`write` → `predict`, output →
`reconcile`). Pending 1.6 was stale on that point, but the feature had never run
over a real link. The new live harness (below) ran it, and found three bugs:

| Bug found live | Fix |
|---|---|
| **Every character typed at a password prompt was drawn** (`read -s`: 9 of 9). Echo-off was inferred from timeouts, but timeouts were only checked when output arrived, and a password prompt sends none. | **Epochs** (Mosh's rule). After Enter, any key that is not predicted, a paste, a mismatch, a resize, or output that moved the cursor with nothing predicted, new predictions are **held**: tracked but not drawn until the server echoes one. A no-echo prompt never confirms an epoch, so nothing typed into it is drawn. An echo from an older epoch cannot reveal a newer one. Held predictions are scored as `withheld`, outside the hit rate, so `sudo` prompts cannot push a session toward the 85% shut-off. Timeouts now also fire from the frame poll, not only on output. |
| **Several backspaces flickered**: 3 typed together scored 1 hit and 2 misses. Arrival was "server cursor at or past the target", which is wrong for leftward moves. | A run of predictions is confirmed when the server cursor is **exactly** where one of them left it and every cell they touched holds the predicted glyph, applied in order. A coincidental cursor match with the wrong cells is neither a confirm nor a miss. |
| **A whole line in one write (snippet, broadcast) skipped the epoch.** The queue (12) filled, and the Enter past the cap was never examined. The echo and the prompt arriving in one chunk then confirmed the epoch, and **7 of 9** password characters were drawn. It only appeared on the fourth live run. | Bytes left over past the cap start a new epoch if any of them is not printable. The regression test feeds the echo and the prompt as one chunk, and fails without the fix. |

Also: the frontend now polls a snapshot as soon as a keystroke's write resolves,
instead of waiting for the next animation frame (§5.4 input path). That removes
up to one frame of delay. It is not measured here, because the harness sees the
backend only.

### The live harness: `crates/app/tests/live_ssh.rs`

`cargo test -p opspilot-app --test live_ssh -- --ignored --nocapture`

It runs unprivileged and self-contained:
- real `sshd` running as the user on a free loopback port;
- a private `ssh-agent` holding a throwaway key;
- a TCP proxy adding 100 ms each way (§4's 200 ms RTT row, without root for `tc`).

It connects through `remote::connect` with **agent auth only**, opens a session
through `registry()`, and types into real shells. It never touches the user's
agent, keys or `known_hosts` (`SSH_AUTH_SOCK` is explicit on every command, and
`OPSPILOT_CONFIG_DIR` points at the temp dir).

Results, three consecutive runs (OpenSSH 10.6, 200 ms RTT):

| | visible P50 | visible P99 | server echo P50 | hit rate |
|---|---|---|---|---|
| bash `--norc` | 53–83 µs | 202–203 ms | 203 ms | **100%** (73/73) |
| zsh `-f` | 53–116 µs | 202–203 ms | 203 ms | **100%** (19/19) |
| the user's own zsh (syntax highlighting + autosuggestions) | 65–101 µs | 204–205 ms | 204 ms | **100%** (57/57) |
| password prompt (`read -s`) | — | — | — | **0 of 9** characters ever drawn |

- Connect plus agent auth through the proxy: 1.04 s (about 5 round trips).
- **"Visible" is the backend half** (keystroke → glyph in the snapshot). The
  webview adds one IPC round trip and a canvas paint, which this harness cannot
  see. §4's "< 8 ms" row is therefore met by the backend with a wide margin and
  is still **unproven end to end**.
- **P99 is one round trip by design.** The first key of each line is held until
  the server echoes it; every key after it is instant. §1's "20–40×" holds for
  typing within a line. For the first key of each line, no terminal can do
  better than the server's echo without risking a drawn password.

### Verified

- `cargo test --workspace`: **299 passed**, 0 failed.
- Clippy `-D warnings`, typecheck, lint, build, `test:keys`: clean.
- core-term: 12 new predictor tests (7 for epochs, 4 for exact-position
  arrival, 1 for the queue cap). The 400-case convergence proptest still passes.
- core-ssh: agent auth against a **real `ssh-agent` process**, covering an empty
  agent, a loaded agent, the status listing and no agent.
- app: method order with and without the agent; `use_agent` TOML round trip;
  held predictions are not in the snapshot and do not move the caret.

### Not done

- End-to-end keystroke → pixel timing (needs the webview; see 1.1).
- `Kind::CursorOnly` still has no emitter beyond Left/Right.
- A hardware-key agent (touch to sign) was not tested. The 90 s auth timeout
  is sized for it.

---

## Phase 19 — The SFTP transfer queue (§6)

`crates/core-sftp` was a 20-line skeleton. It is now the transfer engine, and
the app has a queue in front of it.

### What picks the method

| Request | Method | Why |
|---|---|---|
| one file (download) | **pipelined**: AIMD request window with slow start and a BDP ceiling (§6.1) | bounded by requests in flight, not bandwidth |
| one file ≥ 16 MiB (upload) | **striped** over 8 channels, disjoint ranges of one part file | the *server's* per-channel window caps one channel (see below) |
| directory, > 200 files averaging < 1 MB, server has `tar` | **tar-pipe** over an exec channel (§6.2) | ~4 round trips per file is the cost, not bytes |
| any other directory | **parallel** SFTP, 4–8 channels, largest file first | one big file must not hold up the rest |

The method is shown on every transfer row (§6.2). If `tar` is missing on the
server, the transfer falls back to parallel SFTP and the row says so.

### Measured (`core-sftp/tests/sshd.rs`, OpenSSH 10.6, 40 ms RTT proxy, no loss, release)

| | result | vs WinSCP-style |
|---|---|---|
| download, 1 request in flight × 32 KiB (WinSCP) | 0.7 MB/s | base |
| download 32 MiB, pipelined | 75 MB/s (includes ~6 RTT setup) | **104×** |
| download 128 MiB, pipelined | **195 MB/s**; a fixed 128-request window: 220 | |
| upload 128 MiB, 1 channel / 4 stripes / **8 stripes** | 41 / 96 / **129 MB/s** | |
| local ceiling (no added latency) | download 744–832 MB/s, upload 412–616 MB/s | |
| 2,001 × 4 KiB, sequential SFTP | 6 files/s (10k files: 29 min) | base |
| 2,001 × 4 KiB, 8 parallel channels | 46 files/s | 8× |
| 2,001 × 4 KiB, **tar-pipe** | **2,853 files/s** (10k files: 3.5 s) | **~500×** |

§4's "≥ 30× WinSCP" for small files is met by a wide margin. §4's "≥ 90% of
1 GbE" is **not proven**: there is no 1 GbE link here. Through 40 ms of
latency the download reached 195 MB/s and the striped upload 129 MB/s.

### Found by measuring, not by reasoning (CLAUDE.md §18.23)

- **The SSH channel window was the download cap, not the request window.** At
  8 MiB, downloads stopped at 81 MB/s whatever the SFTP window; a deep fixed
  window of 128 requests scored the same. russh refills a window only below half,
  so the window must be about 2× the BDP. It is now 32 MiB: 219 MB/s.
- **The server's 2 MiB window caps one upload channel** at about 50 MB/s at
  40 ms. The client cannot change it, but every channel has its own, so large
  uploads are striped over 8 channels.
- **Slow start.** Growing +1 per round trip from 16 took about 18 round trips to
  reach 34. Doubling per round trip until the first stall, still under the BDP
  ceiling, took a 128 MiB download from 110 to 195 MB/s.

### Bugs found on the way

- **`russh-sftp`'s `FileAttributes::default()`** is size 0, uid/gid 0, mode
  `0o777|DIR` (§18.22). As `fsetstat` attributes it **truncated every upload to
  zero bytes**; the round-trip test caught it. On `open`/`mkdir` it asks for
  mode 0777. A check that directories are not world-writable did **not** fail
  with the bad value, because OpenSSH applies its umask, so that check was
  removed rather than kept as false assurance.
- **`~` in a key path was never expanded** (pre-existing). A saved identity with
  `key_path = "~/.ssh/id_ed25519"` (the model's own example, and §12.2's rule)
  could not connect. Found in the GUI test; fixed with `paths::resolve`, with a
  test.
- **The SFTP pane hid file names** below ~400 px width (pre-existing): the `1fr`
  name column collapsed under the fixed columns. Names now keep 120 px and the
  pane scrolls sideways.
- **The pane did not refresh after a fast transfer.** A job that started and
  finished between two idle (1 Hz) samples was never seen running. Any job
  reaching `done` now counts, and queueing a transfer wakes the list at once.

### Safety

- Every file goes to `.name.opspilot-part` and is renamed into place
  (`posix-rename@openssh.com` when replacing remotely). A failed or cancelled
  transfer leaves nothing under the real name; tested by cancelling mid-file,
  both in the suite and from the UI.
- An existing target is refused unless "replace" is chosen. A directory then
  merges.
- **Server-supplied names are validated** before use: `..`, `/`, `\`, NUL
  and empty names are refused and counted. Tar extraction uses `unpack_in`; a
  test feeds a hostile archive with `../escaped.txt` and asserts it lands
  nowhere.
- Every path in a remote command is shell-quoted; a test round-trips
  `$(rm -rf ~)`-style strings through a real `sh`.
- Symlinks are never followed when uploading. The SFTP path skips them
  (counted, shown); tar preserves them as links.
- Mode bits are kept without setuid/setgid. Windows sends no mode rather than
  inventing one (§18.11).

### The queue (`crates/app/src/transfers.rs`, `TransferList.tsx`)

- At most 2 jobs run at once; the rest queue in order. Commands are in-memory
  only (§18.19). Progress is atomics sampled at 10 Hz while active and 1 Hz idle
  (§13.1 rule 8). Rates are a 1 s EWMA (§14.6), with ETA, files done and
  skipped counts.
- Directories upload and download. The old whole-file, in-memory
  `sftp_download`/`sftp_upload` commands are gone.
- Transfers survive a webview reload: the queue is native and the new page
  lists it.

### Verified

- `cargo test --workspace`: **320 passed**, 0 failed. New: AIMD (6),
  tar/quoting/hostile archive (5), names/paths (4), the queue (2), key-path
  expansion (1), and against real OpenSSH (4): file round trip with mode,
  striped upload, directory round trip by both methods, overwrite refusal, and
  cancel. The OpenSSH tests skip where `sshd` is not installed.
- Clippy `-D warnings`, typecheck, lint, build, `test:keys`: clean.
- **Real app**, nested Xephyr display, isolated `HOME` and config, against a
  throwaway sshd behind a 40 ms (then 1 s) RTT proxy:
  - a 191 MB upload ran "striped · 8 channels" in 1.2 s (`cmp` identical, mode
    and mtime kept);
  - a 2,500-file directory ran "batched · tar" in 0.7 s (`diff -r` identical);
  - downloading it onto an existing copy failed with "already exists". "Replace"
    merged it and restored a deliberately modified file;
  - cancel mid-download (at 1 s RTT) left neither the file nor its part file.
- `tests/live_ssh.rs` (predictive echo) moved to the shared fixture crate
  `crates/test-sshd`. It still passes with the 32 MiB window.
- **Binary size: 22.9 MB (Phase 15) → 24.2 MB** (release, fat LTO, stripped).
  That +1.3 MB is everything since Phase 15, not this phase alone; most of it is
  likely core-sftp and `tar`, but no per-crate breakdown was taken (`cargo
  bloat` is not installed).

### Not done

- **Resume** (§6.5: persist offset and prefix hash, verify before resuming).
  A cancelled or failed transfer restarts from zero.
- **Merkle compare / delta sync** (§6.5), **speculative listing prefetch**
  (§6.6), **io_uring/IOCP zero-copy and a buffer pool** (§6.3). Each chunk is
  one allocation; the local ceiling above suggests it is not yet the limit.
- Parallel SFTP for many small files is ~4 round trips per file per channel.
  Tar-pipe covers the case that matters; a server without `tar` gets 8×, not
  500×.
- The helper agent (§6.7).
- Packet loss is not simulated; the proxy only delays. The flaky-link
  behaviour of AIMD's halving is unit-tested, not measured.
- Panes do not watch for outside changes (pre-existing). They refresh after a
  transfer or on "refresh".

---

## Phase 20 — GPU renderer (2026-11-26)

`crates/render`, previously a placeholder, is now the renderer the
`spike/wgpu-compose` measurements chose (docs/spikes/wgpu-compose.md on that
branch), and on Linux/X11 the app draws terminals with it (part 2 below).

### What is in it

- **Font** (`font.rs`). JetBrains Mono NL, four faces, subset with fontTools to
  the terminal's ranges and embedded (about 130 KB each, 4 faces; OFL text
  alongside). No system lookup on cold start, and the cell size is identical on
  every machine. Cells are **whole pixels** (7.5 px advance → 8), so every
  column starts on a pixel and one bitmap serves every column. Hinted with
  swash.
- **Box drawing and blocks drawn to the cell** (`boxdraw.rs`). The font's box
  glyphs span the em box; at line height 1.36 vertical strokes stop short of the
  row and `tree` output comes out dashed. All light/heavy lines and junctions,
  half lines, `═ ║` and every block element are drawn; dashes and the other
  double-line junctions still come from the font.
- **Atlas** (`atlas.rs`). 2048² R8, shelf-packed. ASCII pre-rasterized at start
  (§5.7) with a direct-indexed fast path. No CPU mirror of the texture (4 MiB
  saved), only an upload queue. A full atlas clears and bumps an epoch, and
  frames built against the old epoch rebuild.
- **Frame** (`frame.rs`). Rows → instanced quads, cached per row; only damaged
  rows rebuild (§5.3). Layers: background runs (one quad per run, not per
  cell), glyphs and underline/strike, cursor (block with the glyph inverted;
  hollow when unfocused), selection. Reverse, dim, hidden, bold, italic, wide
  characters.
- **GPU** (`gpu.rs`, `shader.wgsl`). One pipeline, one draw call. Glyph quads
  are pixel-aligned, so the shader fetches atlas texels directly with no
  sampler. **One colour pipeline for every target** (non-sRGB format, gamma-space
  blending like canvas and cairo). That fixes the brightness jump the spike
  showed between its two paths. The alpha channel is blended so it stays 1;
  plain alpha blending leaves `a² + 1 − a` under glyph edges, which the overlay
  path would show as see-through text. Offscreen read-back for tests and the
  overlay path.

### Measured (`benches/frame.rs`, machine at load 25)

| | time |
|---|---|
| full rebuild, dense 200×50 coloured screen | 0.46 ms |
| one row changed (typing) | 9 µs |

### Verified

- 22 unit tests, plus 3 GPU tests on the real adapter (`tests/gpu.rs`: VT bytes
  → screen → pixels): cell placement, colours, backgrounds, box drawing reaching
  the cell edge, cursor inversion, selection blend, and **a damage-driven frame
  is byte-identical to a full rebuild**. The GPU tests skip without an adapter.
  `OPSPILOT_RENDER_DUMP=dir` writes the frames for a look by eye.

### Found on the way: every webfont was missing a–z

`scripts/vendor-fonts.mjs` picked fontsource's `latin-ext` slice as the "widest"
subset. That slice has **no basic Latin at all**, so all seven bundled faces had
no a–z and **the whole UI has been rendering in the system fallback face**
(DejaVu here), with no error. The script now vendors `latin` and `latin-ext`,
and `fonts.css` declares both with fontsource's unicode-ranges. JetBrains Mono
now comes from `crates/render/fonts/`, so the canvas and GPU terminals draw the
same face, and both have box drawing (Pending §5 item 1, now fixed). Checked by
reading each file's cmap and by a `vite build`. **Not checked by eye in the
running app.**

### Part 2: wired into the app (`crates/app/src/gpu_term.rs`)

- **One native child window per terminal pane**, created through GDK on the
  main thread, click-through (empty input region), with a wgpu surface on it.
  Present mode Immediate, else Mailbox, else Fifo.
- **One render thread** (§13.2) owns the device, the atlas and every surface.
  It draws a pane when its session's generation moves: coalesced to 60 Hz for
  output (§5.4), and polled every 2 ms for the 50 ms after a keystroke
  (`session_write` wakes it), so an echo or prediction is drawn as soon as it
  exists. Idle cost measured: **0.4% of a core** for the thread.
- **The webview keeps input, selection and layout.** Each frame it compares
  the pane's rect, focus, selection, font and overlay state with what it last
  sent, and calls `gpu_pane_set` only on a change. Cell metrics come from Rust
  (whole pixels) and the canvas lays out the same grid.
- **Overlays**: elements marked `data-overlay` (palette, connect dialog, host
  editor, vault, tooltip) hide the child window of any pane they intersect.
  The canvas, which keeps its rows current underneath, paints while hidden.
  No compositing hook was needed, so the spike's C2 path is not used.
- **Two damage consumers.** `Session`'s dirty flags became a per-consumer
  bitmask (webview, GPU). With one shared flag, whichever consumer polled first
  took the damage and the other drew stale rows. Test:
  `snapshot_and_paint_do_not_steal_each_others_damage`.
- **Falls back** to the canvas on Wayland, with no adapter, or off Linux, and
  says why on stderr (`gpu terminal off, using the canvas: ...`).

Checked in the real app on a nested Xephyr display (lavapipe, since Xephyr has
no DRI3): colours, bold, underline, `ls --color`, `tree` joining across rows,
drag selection drawn by the GPU, hollow cursor when unfocused, palette and
dialog on top with the canvas showing under them, the child following window
grow and shrink with the shell reflowing, and a 300,000-line `seq` ending on the
right final state. Found and fixed on the way: the canvas selection layer could
keep a stale highlight while hidden (now resynced whenever the canvas takes
over). Workspace: **345 tests pass**, clippy `-D warnings`, typecheck, and lint
are clean.

### Keystroke → pixel, the real app on the real display

Your X11 desktop (xfwm4 compositing, 1920×1080 at 60 Hz, Intel Iris Xe), a
local zsh, debug build. Harness: `crates/app/src/latency_probe.rs`, built with
`--features opspilot-app/latency-probe` and run with
`OPSPILOT_LATENCY_PROBE=1` (add `OPSPILOT_GPU_TERM=0` for the canvas). It
dispatches real GDK key events to the app's own window with
`gtk_main_do_event`, so nothing goes to the desktop. A second X connection
polls a pixel in the cursor's cell. The path measured is GTK → WebKit → the
page's keydown → IPC → PTY → zsh echo → parse → draw → present → pixel.
150 samples per run after 10 warm-up, keys 120–320 ms apart.

| | p50 | p90 | p99 |
|---|---|---|---|
| canvas (the old path) | 24.57 ms | 33.78 | 44.70 |
| GPU, first cut (echo polled every 2 ms) | 7.16 ms | 9.39 | 18.82 |
| **GPU, echo wakes the render thread** | **5.36 ms** | 8.27 | 14.87 |
| *(a second run of the same)* | 5.42 ms | 8.50 | 20.61 |

**4.6× faster than the canvas at p50.** It still misses §4's 5 ms p50, by
0.4 ms, and its 12 ms p99. Where the time goes (p50, final build):

| stage | p50 | p90 |
|---|---|---|
| key → `session_write` (GTK, WebKit, JS, IPC) | 1.53 ms | 2.01 |
| write → draw (PTY, shell echo, parse, wake) | 0.23 ms | 0.37 |
| draw → present (build, submit) | 0.91 ms | 1.10 |
| present → pixel (driver, X server) | 2.72 ms | 5.28 |

- **Found by the breakdown:** write → draw was a steady 2.1 ms. That was the
  render thread polling for the echo every 2 ms, while the shell echoes in
  about 0.2 ms. Session readers now wake the render thread directly
  (`gpu_term::output`), but only for 50 ms after a keystroke, so floods stay
  coalesced to frames. Saved 1.8 ms.
- present → pixel and its tail are the driver and X server. Not investigated.

### Keys handled natively (`crates/app/src/keys.rs`, `gpu_term::on_key_press`)

The 1.5 ms of WebKit, page and IPC is gone for terminal keys. While a GPU pane
is focused **and not covered by an overlay**, a GTK `key-press-event` hook on
the toplevel runs before WebKit. It encodes the key in Rust, writes it to the
session (every pane in the tab while broadcasting) and wakes the renderer. It
swallows the matching key release, and tells the page (`term-native-key`) so
typing still clears the selection.

- `keys.rs` is a rule-for-rule port of `src/lib/keys.ts`, and its 8 tests are
  the TS suite's cases. Both encoders stay: the page still handles the canvas
  path and every key Rust passes on.
- **Passed to the page untouched:** every Ctrl+Shift chord (palette, copy,
  paste, close tab, and Ctrl+_ undo), Ctrl+PageUp/PageDown, Shift+PageUp/PageDown,
  Shift/Ctrl+Insert, Ctrl+Alt+Left/Right, Alt+Shift splits, Ctrl+digits and
  zoom, Super, dead keys, and anything without a known encoding.
- **Overlay rule.** The native path stops while a palette or dialog covers the
  pane, and the page decides where keys go. Focus reaches Rust asynchronously
  from the page; without this rule, a key typed in that window could go to the
  shell behind a dialog. One unexplained run in the nested display, never
  reproduced in three exact replays, typed into the shell behind an open
  palette. The rule makes that impossible from the native side.
- Checked in the real app (nested display): typing, Ctrl+C interrupting
  `sleep`, Up recalling history, `cat -v` showing `^[OA` / `^[[A` under DECCKM
  on and off, `^[[1;5D`, `^[[Z`, `^[b`, the palette chord, typing in the palette,
  selection cleared by a keystroke. (F5 arrived as Alt+F5 on *both* the native
  and the old JS path there: xdotool reaches F5 through an Alt level in that
  server's keymap.)
- **Measured on the real display, machine idle, two runs:**

  | | p50 | p90 | p99 |
  |---|---|---|---|
  | GPU, before native keys | 5.36 ms | 8.27 | 14.87 |
  | **GPU, native keys** | **5.04 ms** | 7.85 | 19.42 |
  | *(a second run of the same)* | 5.03 ms | 6.22 | 23.88 |

  | stage | before | native keys (p50, p90) |
  |---|---|---|
  | key → write | 1.53 ms | **0.10**, 0.12 |
  | write → draw | 0.23 ms | 0.26, 0.36 |
  | draw → present | 0.91 ms | 0.83, 1.23 |
  | present → pixel | 2.72 ms | **3.87**, 6.52 |

  Key → write fell by 1.4 ms; p50 improved only 0.3 ms because present →
  pixel came out 1.1 ms higher in the same runs. That stage follows the GPU's
  power state (next section), so the 0.3 ms is within run-to-run drift. The
  p99 tail is all present → pixel. (A run during a game read 11.69 ms and is
  discarded.)
- **Harness gotcha:** run it with a scratch `HOME` that has a `.zshrc`. With an
  empty `HOME`, zsh opens its new-user wizard instead of a prompt, nothing
  echoes, and the probe reports "keys never showed up".

### Chasing present → pixel (what did not help, and why the numbers drift)

With native keys the app's own stages total about 1.1 ms; the rest is after
`present`. Tried, all on the real display, debug build unless noted:

| change | p50 | verdict |
|---|---|---|
| Immediate (current) | 5.0 ms | keep |
| Mailbox | 15.3 ms | 3× worse |
| Fifo | 14.4 ms | 3× worse |
| release build (fat LTO) | 5.00 ms | no change: dependencies already build at opt-level 3 in dev |
| pre-wake: a trivial GPU submit when the key arrives | 5.06 / 5.08 vs 4.95 off | no gain, reverted: it moved 0.25 ms from submit onto the render thread just as the echo arrived |
| a 250 µs gap between the probe's `GetImage` polls | 4.97 ms | the probe does not load the X server it measures |

Split further: build 12 µs, acquire 0.1 ms, encode 0.13 ms, **submit
0.42 ms**. Waiting for the GPU before presenting showed it takes about 1.7 ms
to execute a frame of one clear and 2–3 quads at 1006×894: 1.8–2.0 ms cold,
0.9 ms when submitted again immediately.

**The GPU's clock is most of the gap, and area is most of the rest.** With
the GPU free to idle at 100 MHz, each keystroke's frame is too short a burst
for the driver to clock up for, and the results drift by more than any change
above. Early runs suggested latency scales with surface area (2.64 ms p50 at a
small window, 5.80 at a large one); an hour later, with the GPU idling at
100 MHz instead of 1,100, interleaved runs gave the reverse (small 6.8 ms,
default 5.3). Pinning the minimum clock at 800 MHz (`gt_min_freq_mhz`, root,
a diagnostic only) made it repeatable, two interleaved rounds each:

| GPU floor 800 MHz | p50 | p90 | present → pixel p50 |
|---|---|---|---|
| default, 1006×894 terminal | 3.16 / 3.19 ms | 3.60 / 4.13 | 1.86 / 1.87 ms |
| small, 366×494 terminal | 2.12 / 2.11 ms | 2.52 / 2.52 | 0.82 / 0.85 ms |

So about 2 ms of the 5 ms p50 is the GPU clock, and about 1 ms of what remains
at the default size is drawing and copying the whole surface for a one-cell
change. The reversal at 100 MHz fits the clock governor upclocking for large
frames and never for small ones; that is a guess, not measured. **p99 is
14–22 ms in every configuration**, clock and size alike, and is all present →
pixel: a separate tail, not yet explained. Compare only interleaved A/B runs,
log `/sys/class/drm/card0/gt/gt0/rps_cur_freq_mhz` with them, and check
nothing else is busy (a browser tab at 180% CPU spoiled one round).

Not possible:
- Taking the child out of the compositor: a compositor
  redirects the toplevel together with its children, and xfwm4 unredirects
  only fullscreen windows.

Probe switches (probe builds only): `OPSPILOT_PROBE_PRESENT=immediate|mailbox|fifo`,
`OPSPILOT_PROBE_GPU_WAIT=1` (wait for the GPU before present; reports GPU time),
`OPSPILOT_PROBE_POLL_US=n`, `OPSPILOT_PROBE_WINDOW=WxH`. The probe now quits
through `app.exit`, the normal path: `process::exit` segfaulted in Mesa's exit
handlers in 2 of about 12 runs, while `app.exit` was clean in 15 runs.

### Damage-only presentation (`crates/render/src/present.rs`)

wgpu's surface cannot present part of a window: it treats each acquired image
as uninitialised (so it clears it) and does not expose
`VK_KHR_incremental_present`, which the driver supports. So on Linux/Vulkan
each pane owns its swapchain underneath wgpu:

- The device is opened through wgpu-hal's `open_with_callback`, adding
  `VK_KHR_incremental_present`, then handed back to wgpu.
- `Frame` now tracks damage: rebuilt rows, the cursor's old and new rows, the
  selection's old and new rows, everything after an invalidate. One band,
  full width, widened a row each side for glyph overhang.
- `Renderer::draw_damage` loads the previous canvas, fills the band with the
  background and draws every instance scissored to it.
- `Presenter` copies into each swapchain image the union of damage since that
  image was last shown (a full copy if it is older than 16 frames or new),
  and presents with the band as the region. One small raw command buffer;
  the canvas goes back to the layout wgpu's tracker expects.
- Falls back to wgpu's surface on any error; `OPSPILOT_GPU_DAMAGE=0` forces
  that, for comparison.

Verified: the GPU test `damage_only_frames_match_a_full_redraw` (typing,
backspace, a new line, a selection appearing and moving, the cursor going
unfocused: each damage-only frame equals a fresh full render, pixel for
pixel); in the nested display, typing and editing mid-line, `seq 1 200000`,
colours, `less` and back, a drag selection, focus loss, a split (remount and
resize, two presenters on one device) and closing the tab.

Two bugs found on the way:

- **A window shown again kept stale pixels.** Showing a child queues the map
  on GTK's X connection; the render thread presents on Mesa's. A present that
  reaches the server first is dropped. wgpu's surface hid this by repainting
  everything on the next change; a damage-only window never repairs it. Now
  the map is followed by a round trip (`show_synced`) and an `Exposed`
  message, and the next **two** frames present the whole window: with one,
  2 of 6 palette open/close cycles still left the window's own pixels stale;
  with two, 0 of 12. The old path measured 6 of 6 clean (it presented twice
  by accident).
- **Closing or splitting a pane segfaulted** (libgdk-3), on both paths and
  since part 2. `gdk_window_destroy` unrefs the window, and the gtk-rs binding
  passes a borrowed pointer, so dropping our handle freed it twice. Fixed in
  `destroy_window`. Splits had not been exercised on the GPU path before.

**Latency.** Four interleaved pairs on the real display, stock clock floor
(100 MHz), machine quiet (load about 1):

| | p50 of each run | median | p90 range |
|---|---|---|---|
| **damage-only** | 4.71, 4.68, 4.65, 4.67 ms | **4.67 ms** | 5.5–7.4 |
| wgpu surface | 4.99, 4.93, 4.99, 4.95 ms | 4.97 ms | 5.5–8.2 |

Damage-only won all four pairs, by about 0.3 ms, all of it in present →
pixel (3.58 against 3.77 ms). Smaller than the ~1 ms the pinned-clock area
test suggested: at the stock floor most of what is left is the idle GPU, not
area. It puts p50 under §4's 5 ms in every run. **p99 is unchanged** (14–26 ms
on both paths): that tail is not area either.

An earlier set of pairs, with the machine busy (npm, a VM, another WebKit
app; p90 18–26 ms everywhere), gave medians of 5.75 against 6.4 ms and one
pair the other way: the same direction, unusable as a number.

### The p99 tail is after the present, in the X server

Three runs of 700–800 samples with a per-sample CSV
(`OPSPILOT_PROBE_SAMPLES`, `OPSPILOT_PROBE_CSV`, which also records wall time
and the probe's last `GetImage` round trip):

- **The app's own part is small at p99.** Key → present: 1.09–1.14 ms p50,
  **1.75–2.67 ms p99**, 5.6 ms max. Every slow sample is slow in present →
  pixel.
- **Our GPU work is not what waits.** With `OPSPILOT_PROBE_GPU_WAIT=1`, slow
  samples finished our render *sooner* than fast ones (1.2 against 2.0 ms);
  the time went after the present, in the X server answering the probe's
  pixel read 7–16 ms late.
- Slow samples were not tied to the key, the idle gap before it, or a 60 Hz
  phase; they came about every 5 s, sometimes in bursts.
- Other apps' GPU use (fdinfo, 50 ms timeline) was only 1.5× higher around
  slow keystrokes than fast ones, 2.5% against 1.6% of the GPU: not it.
- **With OpsPilot closed, the X server stalls the same way.**
  `scripts/x-stall-monitor.c`, 90 s of 1 ms round trips: a plain round trip
  never took over 10 ms (max 8.3), but one that syncs with the GPU (a 1×1
  `GetImage`, the probe's kind of read) took over 10 ms **21 times**, max
  16.1 ms, in bursts. Same size and rate as the probe's tail.

So on this desktop (Xorg with glamor, xfwm4 compositing, Iris Xe at its idle
clock) anything that waits for the GPU through the X server regularly waits
10–16 ms.

**But that is not the whole tail.** Compositor on/off, interleaved, 45 s of
the monitor and 300 probe samples each, with other work going on:

| | X stalls >10 ms | probe p50 | p90 | p99 |
|---|---|---|---|---|
| compositor on, round 1 | 71 | 4.38 ms | 7.0 | 14.2 |
| compositor off, round 1 | 52 | 5.49 ms | 16.2 | 23.4 |
| compositor on, round 2 | **1** | 4.60 ms | 7.8 | **19.4** |
| compositor off, round 2 | 377 | (probe window was behind other windows) | | |

- **Compositor off is worse.** The minimum rose from about 1.8 to 4.3 ms and
  p90 to 16 ms: without a compositor the X server ties windowed presents to
  vblank. Not a fix, and not something to tell users.
- **In round 2 the monitor saw one stall in 45 s and the probe's p99 was
  still 19.4 ms.** So general X server stalls make the tail worse when they
  happen, but part of it is specific to presenting our window (Present on a
  redirected window, both present paths). Not yet explained.

The app's own share stays under 3 ms at p99 in every run.

Dead end, recorded so it is not repeated: splitting present → pixel with
the Present extension's `CompleteNotify`. For an immediate copy to a window,
Xorg stamps the event with the last vblank's time, not the time it executed
the copy, so only presents that a vblank happened to follow could be
matched (11–20% of them, biased slow), and the split was meaningless. The
code was removed.

### Not done

- **§4 latency budget:** p50 is met: 4.67 ms against < 5 ms at the GPU's
  stock clock floor, with damage-only presentation. **p99 end to end is
  not:** 14–26 ms against < 12 ms. The app's share of p99 is 1.8–2.7 ms; the
  rest is after the present, in the X server: partly its own GPU-synchronised
  stalls (present with the app closed), partly the present path for our
  window, not yet explained (see "The p99 tail is after the present").
  Compositor off is worse. Untested: a Wayland session.
- **Release binary size cost unmeasured** (wgpu + swash + fonts).
- Wayland, Windows and macOS use the canvas. HiDPI is coded (device-pixel
  surfaces and font size) but untested.
- Refresh rate is fixed at 60 Hz (§14.6 wants the panel's rate).
- The webview still polls snapshots and serializes changed rows while the GPU
  path draws. It needs them for selection text and the overlay fallback; a
  text-only snapshot would cut that cost.
- **No fallback fonts.** Anything outside the subset (CJK, emoji) draws as
  U+FFFD. Needs a lazy system lookup, plus an RGBA atlas for colour emoji.
- Rounded corners are drawn square; Powerline separators come from the font.
- Cell metrics must come from Rust once the GPU path is live. The canvas
  measures fractional advances (7.5 px) and would disagree on column count.

---

## Pending

One authoritative list, replacing the old "Open items" section.

The per-phase **NOT done** blocks above are historical — they record what was
missing *when that phase shipped*, and several have since been done (reflow,
wide characters, scrollback, tabs, splits, the command palette, throughput
rates). This section is the current state; where the two disagree, this wins.

Ordered by what it costs to not have.

### 1. Blocking a `CLAUDE.md` claim

| # | Item | Spec | Why it is not done |
|---|---|---|---|
| 1.1 | **wgpu renderer.** `crates/render` is a 21-line stub; the terminal draws to a 2D canvas. | §2, §7, Step 8 | D-19. Glyphs are rasterised by the browser on the UI thread, which is the exact coupling §3 forbids. **This is why the §4 rows for keystroke latency and the 200 MB `cat` cannot be claimed at all.** The engine behind it measures 47 MiB/s; the ceiling is the canvas. |
| 1.2 | **Splits are one canvas per pane**, not one surface with scissor rects. | Step 9 | Falls out of 1.1. Nothing above `PaneTree` changes when the renderer lands. |
| 1.3 | **Benchmarks need re-measuring.** `parse_and_grid` gave 20.5 / 10.0 / 15.1 / 18.2 MiB/s across four runs of *identical code* — up to 2× variance. | §4, §14 | The machine is not quiet. The earlier 47 MiB/s figure and the later 10–18 range cannot be reconciled from here, and neither is reported as fact. `parse_only` was stable throughout. |
| 1.4 | **No PGO or BOLT.** | §14.4 | Needs CI, which does not exist. |
| 1.5 | **No `vttest`/`esctest` corpus.** The 86 parser tests are hand-written. | §16 | The real conformance vectors are a download that was never authorised. |
| 1.6 | **Predictive echo: end-to-end pixel timing.** Wired and measured live over 200 ms RTT (Phase 18): backend P50 ~80 µs, 100% hit rate, no password characters drawn. The webview's IPC + paint share of §4's < 8 ms is not measured. | §1, §5.5 | Needs the renderer work in 1.1 or a webview-side probe. |
| 1.7 | **Scrollback reflow.** Resize reflows the viewport; history is not re-wrapped. | §5.6 | Needs the lazy 1,000-row chunking §5.6 describes. |

### 2. Security — known residual gaps

| # | Item | Spec | Status |
|---|---|---|---|
| 2.1 | **Secrets broker is not a separate process.** | §12.3 | D-23. Mitigated, not solved: no command returns a secret, so a webview XSS cannot exfiltrate one — but it can still list names, overwrite them, and connect as the user, and nothing contains a compromise of the native side. |
| 2.2 | **No `mlock`/`VirtualLock`.** Memory is zeroized but not page-locked. | §12.3 | A derived key can reach the page file. Needs platform bindings `core-store` does not have. |
| 2.3 | **No OS-keychain convenience wrap.** | §12.3 | D-22. Deliberate. The master password is typed every unlock (15-min idle timeout). Adding the wrap later is additive; an unopenable vault has no recovery, so this is the safe direction to be incomplete in. |
| 2.4 | **No keyboard-interactive/MFA.** Agent auth is done (Phase 18). | — | Not started. |
| 2.5 | **No jump hosts.** `Host::jump_host` is in the model and always `None`. | — | Nothing reads it. |

### 3. Features with no surface yet

| # | Item | Spec |
|---|---|---|
| 3.1 | **Module surfaces: REST client, Log viewer.** Docker, Kubernetes and Databases are built (Phase 15). | Steps 12–13 |
| 3.2 | **Database TLS** (D-30), CSV export, schema browser/autocomplete. Connections are plaintext; the panel warns for remote hosts. | §11 |
| 3.11 | **Kubernetes protobuf** (D-29) and further kinds, events, port-forward. | §10 |
| 3.3 | **Transfer resume and sync** (§6.5: resumable offsets with prefix hash, Merkle compare, delta). The queue, pipelining, striping, tar-pipe and directory transfers are done (Phase 19). | §6.5 |
| 3.4 | **LinkStrip** — the 2px per-tab RTT sparkline. **Blocked, not merely undone:** russh exposes no way to time a round trip, and drawing it from the connect time would be decoration pretending to be telemetry. | §0.4 |
| 3.5 | **Cipher/kex readout** shows `n/a`. russh 0.62's `kex_done` takes a `negotiation::Names` that is private, so the override does not compile. | §0.5 |
| 3.6 | **`rkyv` index cache.** Config is reparsed every launch. Irrelevant at current host counts; §12.4 is about 10,000. | §12.4 |
| 3.7 | ~~Hot reload~~ — built (`core-store/src/watch.rs`, polled by generation counter); this row was stale. | §12.2 |
| 3.8 | **Mouse reporting** (`?1000`/`?1002`/`?1006`). Selection and clipboard are done (Phase 14). Without reporting, clicks in htop/mc/vim `mouse=a` select text instead of reaching the program. | — |
| 3.9 | **Selection is viewport-anchored.** It clears on scroll or resize rather than following the text into scrollback, and soft-wrapped lines copy as separate lines. | — |
| 3.10 | **No IME input.** Composition is ignored correctly, but committed CJK text has no path to the PTY: the terminal is a canvas, not an input element. | — |

### 4. Small and self-contained

- `passphrase_ref` is wired end to end but no UI writes one — only `password_ref` appears in `HostEditor`.
- Saved forwards carry an `auto_start` flag that nothing acts on.
- Drag-to-reorder tabs.
- `Kind::CursorOnly` predictions are modelled and reconciled but nothing emits them.

### 5. Environment — blocked on downloads never authorised

1. ~~**Mono fonts have no box-drawing glyphs.**~~ Fixed in Phase 20: JetBrains
   Mono comes from the upstream TTF, subset with fontTools. The same phase
   found the bigger problem, that every face lacked a–z.
2. **Playwright Chromium is not installed**, so `tests/visual/visual-check.spec.ts`
   has **never executed** — including the `document.fonts.check()` assertion that
   would catch item 1. ~120 MB.
3. **Figma screenshot baselines absent.** A manual PNG export from the Figma
   file; the two screenshot tests fail by design until they exist.

### 6. Housekeeping

- Root-level duplicates of `tokens.css`, `fonts.css` and `visual-check.spec.ts`
  should be deleted now that the installed copies are authoritative.

### Done since this list was last stale

`~~No Rust toolchain~~` — rustup 1.29.0, rustc/cargo 1.97.1.
`~~No MSVC linker~~` — the workspace compiles and `target/release/opspilot.exe`
builds and launches.

