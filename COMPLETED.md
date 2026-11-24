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
| 1.6 | **Predictive echo is not in the live path.** `Predictor` exists, is tested, and reconciles — against synthetic output only. | §1, §5.5 | It needs to sit between keystroke and PTY write. Its value starts at real RTT, so on a local PTY it would predict nothing useful. §1's 20–40× claim remains **unmeasured**. |
| 1.7 | **Scrollback reflow.** Resize reflows the viewport; history is not re-wrapped. | §5.6 | Needs the lazy 1,000-row chunking §5.6 describes. |

### 2. Security — known residual gaps

| # | Item | Spec | Status |
|---|---|---|---|
| 2.1 | **Secrets broker is not a separate process.** | §12.3 | D-23. Mitigated, not solved: no command returns a secret, so a webview XSS cannot exfiltrate one — but it can still list names, overwrite them, and connect as the user, and nothing contains a compromise of the native side. |
| 2.2 | **No `mlock`/`VirtualLock`.** Memory is zeroized but not page-locked. | §12.3 | A derived key can reach the page file. Needs platform bindings `core-store` does not have. |
| 2.3 | **No OS-keychain convenience wrap.** | §12.3 | D-22. Deliberate. The master password is typed every unlock (15-min idle timeout). Adding the wrap later is additive; an unopenable vault has no recovery, so this is the safe direction to be incomplete in. |
| 2.4 | **No SSH agent auth, no keyboard-interactive/MFA.** | — | Not started. Agent auth in particular is what most people actually use. |
| 2.5 | **No jump hosts.** `Host::jump_host` is in the model and always `None`. | — | Nothing reads it. |

### 3. Features with no surface yet

| # | Item | Spec |
|---|---|---|
| 3.1 | **Module surfaces: Docker, Kubernetes, Databases, REST, Log viewer.** The module rail routes to placeholders. | Steps 12–13 |
| 3.2 | **`DataTable`** — the shared virtualised table those modules are built on. | Step 12 |
| 3.3 | **Transfer queue.** AIMD pipelining and the tar-pipe path. Directory transfers are **disabled, not silently broken**; single-file transfer works. | §6.1, §6.2 |
| 3.4 | **LinkStrip** — the 2px per-tab RTT sparkline. **Blocked, not merely undone:** russh exposes no way to time a round trip, and drawing it from the connect time would be decoration pretending to be telemetry. | §0.4 |
| 3.5 | **Cipher/kex readout** shows `n/a`. russh 0.62's `kex_done` takes a `negotiation::Names` that is private, so the override does not compile. | §0.5 |
| 3.6 | **`rkyv` index cache.** Config is reparsed every launch. Irrelevant at current host counts; §12.4 is about 10,000. | §12.4 |
| 3.7 | **Hot reload.** Editing `production.toml` in Vim should make hosts appear. §12.2 calls this "a real differentiator and it costs almost nothing" — it needs a `notify` watcher. | §12.2 |
| 3.8 | **Mouse reporting** (`?1000`/`?1002`/`?1006`). Selection and clipboard are done (Phase 14). Without reporting, clicks in htop/mc/vim `mouse=a` select text instead of reaching the program. | — |
| 3.9 | **Selection is viewport-anchored.** It clears on scroll or resize rather than following the text into scrollback, and soft-wrapped lines copy as separate lines. | — |
| 3.10 | **No IME input.** Composition is ignored correctly, but committed CJK text has no path to the PTY: the terminal is a canvas, not an input element. | — |

### 4. Small and self-contained

- `passphrase_ref` is wired end to end but no UI writes one — only `password_ref` appears in `HostEditor`.
- Saved forwards carry an `auto_start` flag that nothing acts on.
- Drag-to-reorder tabs.
- `Kind::CursorOnly` predictions are modelled and reconciled but nothing emits them.

### 5. Environment — blocked on downloads never authorised

1. **Mono fonts have no box-drawing glyphs.** `@fontsource` ships only
   Google-Fonts-derived subsets; none covers U+2500–259F.
   `scripts/vendor-fonts.mjs` warns per face. **Consequence: `systemd` and `tree`
   output renders as tofu.** The fix is the `pyftsubset` command already in
   `src/styles/fonts.css`, needing `pip install fonttools brotli` plus the full
   IBM Plex and JetBrains Mono TTFs.
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

