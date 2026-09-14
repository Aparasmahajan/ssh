# OpsPilot — UI Build Guide

A step-by-step plan for building the OpsPilot interface with Claude Code. Read `CLAUDE.md` first: the performance rules there constrain what the UI is allowed to do.

**Who this is for:** an engineer at a terminal for eight hours, often on two monitors, often in a dark room, who needs to go from "I need to touch that server" to "I'm typing in a shell" in under a second — and who needs to know, at a glance, *which* server they're typing into. Every decision below serves those two jobs.

---

## Part 0 — Design direction

### 0.1 The thesis: a patch panel, not a dashboard

Most infra tools look like web dashboards that happen to contain a terminal. OpsPilot looks like the equipment it manages: rack-mounted hardware. Labeled rows, colour-coded cabling, port faceplates, link-activity LEDs.

This isn't decoration — it's the information model. The PRD's central idea is *one host, many connection types*. A switch faceplate is exactly that: one device, many labeled ports. So the host row **is** a faceplate, and each connection type is a port chip on it.

### 0.2 Palette

Graphite chassis, anodized rails, and the two colours that actually appear on network hardware: amber activity LEDs and cyan link LEDs. Deliberately **not** near-black-plus-acid-green, which is the default every terminal app reaches for.

```css
:root {
  /* Chassis — cool graphite, never pure black (pure black flares in dark rooms) */
  --chassis-900: #14171C;  /* app background */
  --chassis-800: #1B1F26;  /* panel background */
  --chassis-700: #252A33;  /* raised surface, tab bar, cards */
  --rail-600:    #313846;  /* rails, inset wells */
  --bezel-500:   #3A4252;  /* hairlines, port-chip borders, dividers */

  /* Text — cool bone, not #FFFFFF (white on graphite vibrates) */
  --text-hi:     #E8ECF2;
  --text-mid:    #9AA6B8;
  --text-lo:     #667487;

  /* Signal — the LED palette */
  --amber-500:   #F2A93B;  /* PRIMARY ACCENT. Activity, selection, focus. */
  --amber-900:   #4A3411;  /* amber at 12% for fills */
  --cyan-400:    #4FD1C5;  /* link up, healthy, connected */
  --red-500:     #E5484D;  /* fault, disconnected, destructive */
  --violet-400:  #A78BFA;  /* AI-originated content — always visually distinct */

  /* Environment cable-tie colours (left edge of every host row) */
  --env-prod:    #E5484D;
  --env-stage:   #F2A93B;
  --env-dev:     #4FD1C5;
  --env-test:    #A78BFA;
  --env-cust:    #6B8AFD;
}
```

Rules:
- Amber is the accent. Cyan means *link state only* — never use it for a button, or "connected" stops being readable at a glance.
- Environment colour appears in exactly one place per row: a **3px left edge bar**. Never as a row background tint (that destroys terminal contrast).
- Prod is red. Every destructive confirmation in a prod context says the hostname, and the user types it to confirm.

### 0.3 Type

The **IBM Plex superfamily**, chosen because Plex was designed for hardware operator interfaces — the exact register this product is in — plus JetBrains Mono for the terminal buffer only.

| Role | Face | Usage |
|---|---|---|
| Display | IBM Plex Sans Condensed, 600 | Section eyebrows, module titles. Uppercase, `letter-spacing: 0.08em`. |
| UI | IBM Plex Sans, 400/500 | Labels, menus, dialogs, body |
| Data | IBM Plex Mono, 400/500 | Hostnames, IPs, ports, latency, byte counts, port chips |
| Terminal | JetBrains Mono, 400 | Terminal buffer only. **Ligatures off** — `!=` must not render as `≠` in a shell. |

Scale (tight — density matters more than air in an ops tool):
```
11px/16  micro   port chips, status rail
12px/18  small   tree rows, table cells, tab labels    ← workhorse size
13px/20  base    dialog body, inspector
15px/22  large   module titles
20px/26  display command palette input
```

Anything numeric — an IP, a port, a latency, a transfer rate — is set in **IBM Plex Mono with `font-variant-numeric: tabular-nums`**, so digits don't shift width as they tick. A jittering throughput readout is what makes an app feel cheap.

### 0.4 The signature element: the link strip

**A 2px live latency sparkline running along the bottom edge of each session tab.**

It's the one memorable thing in the interface, and it earns its place because it encodes something true and otherwise invisible: per-session RTT over the last 60 seconds.

- Colour by current RTT: `< 50 ms` cyan → `< 200 ms` amber → `> 200 ms` red.
- Flatlines and goes grey the moment the channel stalls, so you see a hung session **before** you type into it and wonder why nothing echoes.
- Renders on the GPU surface with the terminal. Updates at 10 Hz, not per-packet.
- The active tab's strip is 3px and full-opacity; inactive tabs are 2px at 55%.

This is the answer to "which session is lagging?" — a question PuTTY and Termius users answer by typing and waiting.

### 0.5 Layout

```
┌────┬─────────────────────┬──────────────────────────────────────────┬──────────────┐
│ 48 │ WORKSPACE           │ ▌ linux-01  db-02   k8s-stg        + │  │ INSPECTOR    │
│ px │ ─────────────────── │ ▂▃▂▂▃▄▂▂ ──────────────────────────────  │ ──────────── │
│    │ ▌ PRODUCTION        │                                          │ RTT   3.2 ms │
│ ⌘  │  ▌ linux-01    ●    │  admin@linux-01:~$ systemctl status ngi  │ CPU     14%  │
│ ⇄  │    SSH SFTP DB      │                                          │ MEM   2.1/8G │
│ ▣  │  ▌ linux-02    ●    │                                          │ ── ── ── ─── │
│ ⎈  │    SSH SFTP         │                                          │ cipher       │
│ ⛁  │  ▌ pg-primary  ○    │                                          │  chacha20    │
│ ⇅  │    SSH DB           │                                          │ key ed25519  │
│ ⚙  │ ▌ DEVELOPMENT       │                                          │ up    4h 12m │
└────┴─────────────────────┴──────────────────────────────────────────┴──────────────┘
  ⏱ 3.2ms   ↑ 12.4 MB/s   ↓ 840 KB/s   🔒 chacha20-poly1305   ⚡ 7 sessions   ⌘K
```

Four zones, left to right:
1. **Module rail (48px)** — icon-only, vertical. Workspace / Sessions / Docker / Kubernetes / Databases / Transfers / Settings. Active module gets a 3px amber left edge. No labels; tooltips on hover after 600ms.
2. **Workspace tree (260px, resizable, collapsible to 0)** — the faceplate list. Environment groups, host rows, port chips.
3. **Session surface (flex)** — tab strip with link strips, then the active module's canvas. This is the wgpu surface for terminals, SFTP panes, and large tables.
4. **Inspector (280px, collapsible)** — context for the active session. Live metrics, cipher, key type, uptime. Collapsed by default on screens under 1600px.
5. **Status rail (28px, full width)** — RTT, aggregate throughput, active cipher, session count, and the `⌘K` hint.

Panel sizes persist per workspace. `⌘B` toggles the tree, `⌘I` the inspector, `⌘⏎` zooms the session surface to full bleed (the "I'm debugging, get out of my way" state).

### 0.6 The host row — spec it precisely, it's the most-used component

```
 ▌  linux-01                          ●  ⋮
 │   10.20.4.11 · ubuntu 22.04
 │   SSH  SFTP·deploy  SFTP·backup  DB  ⎈
 ↑
 3px env colour, full row height
```
- **Row height 44px** in two-line mode (name + address), 26px in compact mode. Ship compact mode; power users with 200 hosts need it.
- **Name**: 12px Plex Sans Medium, `--text-hi`.
- **Address line**: 11px Plex Mono, `--text-lo`. Truncate middle, not end — `10.20.4.11 · ubuntu…` keeps the useful part.
- **Status dot**: 6px. Filled cyan = connected, hollow `--text-lo` = idle, filled red = last attempt failed, amber pulsing = connecting.
- **Port chips**: 11px Plex Mono uppercase, 3px radius, 1px `--bezel-500` border, transparent fill. On hover: `--amber-900` fill, `--amber-500` border and text. **Each chip is a click target that opens that connection type directly.** This is the whole product thesis made clickable — one click from a host row to an SFTP session on the `backup` identity.
- Named SFTP identities render as `SFTP·deploy`. The PRD's multi-identity-per-host feature is invisible unless the UI shows the identity name, so it always does.
- **Never** a row hover that changes background lightness by more than 4% — it flickers when you drag across a long list.

### 0.7 Command palette (`⌘K`) — the primary navigation

This is how fast users actually work. It must open in under 30ms and match on the *first* keystroke.

- 20px Plex Mono input, results below at 12px.
- Matches hosts, connection types, saved commands, tunnels, K8s contexts, and settings in one list, grouped by kind with a Plex Sans Condensed eyebrow per group.
- Fuzzy-highlight matched characters in `--amber-500`.
- Typing `prod ssh linux` and hitting `⏎` opens an SSH session on the matching prod host. Typing `>` switches to command mode (actions, not objects).
- Recent selections rank first, then fuzzy score. Frecency, not pure fuzzy.
- Esc closes. Never trap focus.

### 0.8 Quality floor (non-negotiable)

- **Keyboard-complete.** Every action reachable without a mouse. Visible 2px `--amber-500` focus ring, `outline-offset: 2px`. Never `outline: none` without a replacement.
- `prefers-reduced-motion: reduce` → all transitions to `0.01ms`. The link strip stops animating and shows a static bar.
- Contrast: `--text-mid` on `--chassis-800` is the floor and it passes 4.5:1. Verify any new pair.
- Transitions are 120ms `ease-out` for hover, 180ms for panels. Nothing longer — an ops tool that animates makes you wait.
- Screen-reader labels on every icon-only control in the module rail.
- Terminal font size, family, and line height are user settings. Respect the OS text-scale factor for chrome.

### 0.9 Matching the Figma file

Figma reference: `https://www.figma.com/design/wkE7H0umKnwHYtijV6oXXo`
· Session View → `?node-id=3-2` · SFTP Dual Pane → `?node-id=8-2`

The Figma file is a static picture. Claude Code cannot open it, measure it, or diff against it — nothing enforces the match automatically. Two mechanisms make it real:

1. **Tokens are the design; Figma is only a rendering of them.** Generate `tokens.css` once (Step 2), import it everywhere, and enforce the no-hex-literals ESLint rule. A wrong colour then becomes a build error rather than something noticed three months later.
2. **Screenshot-diff the built UI against a Figma PNG export** (Step 16). That closes the loop on layout and spacing.

This aesthetic was chosen partly because it reproduces cleanly: flat fills, 1px hairlines, 3px radius, a 3px edge bar, small-caps labels, mono numerics. No gradients, no layered shadows, no organic shapes. It is nearly a 1:1 mapping to CSS.

**Five things break the match. All are avoidable.**

| Failure | Symptom | Fix |
|---|---|---|
| **Fonts fall back** | Everything instantly reads as a generic dashboard — the type carries most of the personality | Self-host woff2 subsets locally. Never a CDN (also required for offline-first). Set `font-display: block` so you never paint a frame in fallback |
| **Line heights left to `normal`** | Vertical rhythm shifts a few px per row; density looks subtly wrong | Set every line height explicitly in px from the §0.3 scale. Figma uses explicit px |
| **Tailwind Preflight leaks** | Rounded corners where the design has 3px; 8px gaps where it has none; default font stack | Override `borderRadius`, `fontFamily`, and button resets in the theme. Audit the base layer once |
| **Hairlines at fractional DPI** | At 125%/150% Windows scaling the 1px borders and 3px env bar blur or vanish | Logical `0.5px` borders behind a `min-resolution` media query. Verify at 100%, 125%, 150% |
| **Link strip parity** | Canvas-drawn in app, SVG in Figma — easy to drift | Same stroke width, colours, and well height (7px active / 6px inactive). Eyeball side by side once |

**Realistic fidelity:** ~90% on palette and typography (mechanical, once tokens are generated), ~75% on layout (screenshot diffing gets most of the rest), near 0% on motion and interaction states until built from the prose specs above — Figma shows one state per component.

**Where Figma is authoritative and this guide is stale:** §0.6 says the host row is 44px two-line. The Figma build is ~64px three-line, because the port chips *are* the product thesis and hiding them defeats the point. Follow Figma. Compact mode (26px, chips hidden) still needs building as a density option.

**What Figma can never show, so build from the prose:** the link strip is a live 10 Hz sparkline (§0.4); the host tree is virtualized over 10,000 rows (Step 5); the terminal and SFTP panes are wgpu surfaces, not DOM (Step 8). Figma shows one frozen frame of each.

---

## Part 1 — Setup

### Step 1: Scaffold

```
Initialize a Tauri v2 project named opspilot with a React + TypeScript + Vite
frontend and a Rust workspace backend.

Rust workspace crates:
  crates/core-term   — grid, VT parser, scrollback
  crates/core-ssh    — transport, channels, connection pool
  crates/core-sftp   — pipelined transfer engine
  crates/core-store  — redb persistence, vault
  crates/render      — wgpu glyph atlas renderer
  crates/app         — Tauri commands, wiring

Frontend: React 18, TypeScript strict mode, Tailwind CSS, zustand for state,
@tanstack/react-virtual for virtualization.

Set the release profile in the workspace Cargo.toml to lto="fat",
codegen-units=1, panic="abort".

Do not write any UI yet. Just the skeleton, and confirm `cargo build` and
`npm run tauri dev` both succeed.
```

### Step 2: Lock the tokens before any component exists

**These files already exist — copy them in, don't regenerate them.** They encode decisions (Preflight overrides, `font-display: block`, subsetting ranges, fractional-DPI hairlines) that are easy to get subtly wrong and hard to notice afterwards.

| File | Destination |
|---|---|
| `tokens.css` | `src/styles/tokens.css` |
| `fonts.css` | `src/styles/fonts.css` |
| `tailwind.config.ts` | project root |
| `visual-check.spec.ts` | `tests/visual/visual-check.spec.ts` |

```
Wire up the provided token files:
  1. Import fonts.css then tokens.css (in that order) in src/main.tsx.
  2. Install the Tailwind config at the project root.
  3. Subset and place the woff2 files in src/styles/fonts/ using the exact
     pyftsubset command in the fonts.css header comment. The mono faces MUST
     keep U+2500-259F or the terminal renders systemd trees as tofu.
  4. Add an ESLint rule that fails on any hex colour literal in .tsx/.ts.
     Every colour comes from a token.

Then run `npx playwright test visual-check` and confirm the "design system
invariants" block passes. The screenshot tests will fail until baselines are
exported from Figma — that's expected at this stage.
```

Two things here do real work. The **ESLint rule** is what stops the palette eroding within two weeks. The **`fonts.check()` assertions** in `visual-check.spec.ts` catch the highest-impact failure mode in this whole design: if IBM Plex silently falls back to `system-ui`, the app looks like a generic dashboard and nothing else in the build will tell you.

### Step 3: Shell layout

```
Build the four-zone AppShell from §0.5: module rail (48px fixed), workspace tree
(260px, resizable 200–480px), session surface (flex), inspector (280px,
collapsible), status rail (28px, full width bottom).

Use CSS Grid. Resizing via a 4px drag handle with a 12px invisible hit area.
Persist panel widths and collapse state to the Tauri store, keyed by workspace.

Keybindings: ⌘B toggles tree, ⌘I toggles inspector, ⌘⏎ zooms the session
surface to full bleed.

The inspector starts collapsed when the window is under 1600px wide.
```

---

## Part 2 — Core components

### Step 4: The host row

```
Build a HostRow component to the exact spec in §0.6.

Props: name, address, os, environment, status
('connected'|'idle'|'failed'|'connecting'), and connections
(array of {kind, identityName?}).

Requirements:
- 3px left edge bar in the environment colour, full row height
- Two-line mode at 44px, compact mode at 26px, controlled by a prop
- Middle-truncate the address line, never end-truncate
- Status dot per the spec, with the connecting state pulsing amber at 1.2s
- Port chips are individually clickable and emit
  onOpenConnection(kind, identityName)
- SFTP chips with an identityName render as "SFTP·deploy"
- Row hover changes background by no more than 4% lightness
- Full keyboard support: row focusable, arrow keys move between chips,
  Enter activates

Then write a Storybook story covering every status × environment × compact
combination, plus a host with four SFTP identities and one with no connections.
```

### Step 5: Virtualized workspace tree

```
Build the WorkspaceTree using HostRow.

Critical: this must handle 10,000 hosts at under 5ms per keystroke (see CLAUDE.md
§3 and §6). Therefore:

- Flatten the tree to a Vec<VisibleNode> on expand/collapse. Never recurse
  during render.
- Virtualize with @tanstack/react-virtual — only visible rows mount.
- Fuzzy filtering happens in RUST, not JS. Expose a Tauri command
  filter_hosts(query, tags) that uses the fzf-v2 bit-parallel prefilter and
  roaring-bitmap tag intersection from CLAUDE.md §6. It returns ordered host
  IDs plus match ranges for highlighting.
- Debounce input 16ms and cancel the in-flight query on each new keystroke.
- Highlight matched characters in --amber-500 using the returned ranges.

Generate a 10,000-host fixture and add a test asserting that
filter-to-repaint stays under 5ms. If it doesn't, the bottleneck is in the
React layer, not Rust — profile before changing the Rust.
```

### Step 6: Tab strip with the link strip

```
Build the SessionTabStrip. Tabs are draggable to reorder, middle-click closes,
⌘1–9 jumps, ⌘W closes, ⌘⇧[ and ⌘⇧] cycle.

Then the signature element from §0.4 — the LinkStrip:
- 2px sparkline along the bottom edge of each tab (3px when active)
- 60 samples of RTT history, updated at 10Hz from a Rust-side AtomicU64,
  NOT one event per packet
- Colour thresholds: <50ms cyan, <200ms amber, >200ms red
- On channel stall, flatline in --text-lo within 2 seconds
- Draw it in a single <canvas> for the whole strip, one 2D context, redrawn on
  a shared 10Hz rAF tick. Do not create one canvas per tab — with 20 tabs
  that's 20 contexts and it will cost you frames.
- Under prefers-reduced-motion, render a static bar coloured by current RTT

Include a tooltip on hover showing min/avg/max RTT over the window.
```

### Step 7: Command palette

```
Build the CommandPalette (⌘K) to §0.7.

Search is a single Tauri command search_everything(query) returning grouped
results: hosts, connections, saved commands, tunnels, k8s contexts, settings.
Ranking is frecency-weighted fuzzy score — recent selections first, then score.

- Must open in under 30ms. Mount it once, hidden, and toggle visibility.
  Do not mount on demand.
- Results virtualized. First keystroke shows results, no debounce on open.
- Matched chars highlighted in --amber-500.
- "> " prefix switches from object mode to action mode.
- ↑↓ navigate, ⏎ activates, ⇥ completes to the highlighted item, Esc closes.
- Restore focus to the previously focused element on close.
```

---

## Part 3 — The terminal (the hard part)

### Step 8: wgpu surface bridge

```
This is the most important step in the project. Read CLAUDE.md §4 in full first.

The terminal must NOT be an HTML element or xterm.js. Create a Tauri child
webview hole / native surface positioned over the session surface region, and
render the terminal from crates/render with wgpu.

Frontend responsibility is only: report the surface rect on resize, forward
keyboard and mouse events, and never touch the byte stream.

Verify before proceeding:
  1. `cat` a 200MB file — sustained 60fps, zero dropped bytes
  2. keystroke-to-glyph under 8ms P50 (measure with a high-speed capture or
     the built-in Ctrl+Shift+P overlay)
  3. resize with 100k scrollback rows does not block over one frame

If any of these fail, stop and fix the renderer. Do not build UI on top of a
terminal that misses its budget — every later feature will inherit the lag.
```

### Step 9: Splits and layouts

```
Implement terminal multiplexing per PRD §4: horizontal split, vertical split,
drag to rearrange, detach, saved layouts.

Model the layout as a binary tree of panes with split ratios. The frontend owns
the tree and sends rects to the renderer; the renderer draws all panes to one
wgpu surface with scissor rects — one surface, not one per pane.

Keybindings: ⌘D vertical split, ⌘⇧D horizontal, ⌘⌥arrows to focus, ⌘⇧arrows
to resize, ⌘⇧B toggle broadcast-to-all-panes.

Broadcast mode must be unmistakable: 2px amber border around every
participating pane plus a persistent amber pill in the status rail reading
"BROADCAST · 4 panes". Typing the same command into four production servers by
accident is the worst thing this app could let a user do.
```

---

## Part 4 — SFTP

### Step 10: Dual pane

```
Build the SFTP dual-pane browser per PRD §5.

Layout: local pane | transfer controls | remote pane, with a synced-scroll
toggle. Below: the collapsible transfer queue.

Both panes virtualized. Sorting happens in Rust on a pre-extracted key array
(CLAUDE.md §5.5) — never sort row objects in JS.

Columns: name, size, modified, permissions. Size and modified in Plex Mono
tabular-nums. Permissions rendered as both rwxr-xr-x and 0755, the octal in
--text-lo.

Identity switcher at the top of the remote pane: a dropdown listing that host's
SFTP identities (deploy / backup / readonly). Switching identities is a
first-class, one-click action — it's the flagship differentiator and it should
not be buried in a settings dialog. The active identity name is always visible
in the pane header.

Read-only identities render the pane header in --cyan-400 with a lock glyph,
and every mutating control is disabled, not hidden. The user should see that
rename exists and that this identity can't do it.
```

### Step 11: Transfer queue

```
Build the TransferQueue.

Per-item: filename, direction arrow, progress bar, rate, ETA, and a
pipeline-depth indicator showing outstanding requests (CLAUDE.md §5.1).
Aggregate header: total progress, combined rate, files remaining.

Progress comes from Rust AtomicU64 counters sampled at 10Hz. NOT one IPC
message per chunk — at 100MB/s with 256KB chunks that's 400 messages/second
and it will melt the frame budget.

Rate readouts use tabular-nums and are smoothed with a 1-second EWMA. A
number that jitters between 94 and 108 MB/s reads as broken even when it isn't.

When the engine switches to tar-pipe mode for a small-file batch, surface it:
a chip reading "batched · 8,412 files" with a tooltip explaining that files are
being streamed as one archive. Users who have waited nine minutes in WinSCP
should be told why this took eleven seconds.
```

---

## Part 5 — Remaining modules

### Step 12: Data tables

```
Build one DataTable component used by Docker, Kubernetes, and the SQL result
grid. Building three tables is how this codebase gets slow and inconsistent.

Requirements:
- Virtualized rows and columns
- Keyed reconciliation by stable ID — patch changed cells only, never rebuild
  (CLAUDE.md §9)
- Resizable, reorderable, hideable columns, persisted per view
- Sort and filter execute in Rust over the columnar buffer (CLAUDE.md §10)
- Row selection: click, shift-click range, ⌘-click multi
- Status cells use the LED palette: cyan running, amber pending, red failed,
  --text-lo stopped
- Cell content is Plex Mono for anything identifier-like or numeric

Test with 5,000 K8s pods churning at 50 updates/second: repaint must stay
under 16ms.
```

### Step 13: Module surfaces

Build in this order, each on top of `DataTable`:

1. **Docker** — image/container/volume/network tables, log viewer, exec terminal reusing the terminal surface.
2. **Kubernetes** — resource tree by namespace, YAML editor (CodeMirror 6, not Monaco: Monaco costs ~2MB and a measurable slice of cold start), describe/events panes, port-forward controls.
3. **Databases** — schema browser tree, query editor with FST autocomplete, result grid, explain-plan view.
4. **REST client** — collection tree, request builder, response viewer with a diff mode.
5. **Tunnels** — a table of forwards with live health dots and a sparkline per tunnel reusing `LinkStrip`.
6. **Log viewer** — virtualized, mmap-backed, regex highlight, time filter.
7. **Scripts, monitoring, secrets vault.**

### Step 14: AI surfaces (last)

```
Add optional AI per CLAUDE.md §13. It goes last so nothing depends on it.

Every AI-originated element is visually distinct: --violet-400 accent, and a
small ✦ glyph. A user must never mistake generated output for real system
output — that is a safety property, not a style preference.

Before any request leaves the machine, show a redaction preview: the exact
text to be sent, with redacted spans struck through in --red-500. The user
confirms once per session, or per request if they prefer.

Suggested commands land in the prompt line unexecuted, with the run affordance
requiring a separate deliberate action. Never auto-run.

When AI is disabled in settings, these surfaces are absent, not greyed out.
```

---

## Part 6 — Verification

### Step 15: The checklist

Run before every release. Failing any line blocks the release.

**Performance**
- [ ] Cold start under 300ms (measure 10 runs, take P90)
- [ ] Keystroke-to-glyph under 8ms P50, 16ms P99
- [ ] `cat` of 200MB holds 60fps with zero byte loss
- [ ] 10,000-host tree filters in under 5ms per keystroke
- [ ] 5,000-pod table repaints under 16ms at 50 updates/sec
- [ ] Idle RSS under 150MB with 5 sessions
- [ ] No IPC message rate above 20/sec for any progress indicator

**Accessibility**
- [ ] Every action reachable by keyboard; tab order is logical
- [ ] Visible 2px amber focus ring on every interactive element
- [ ] `prefers-reduced-motion` honoured, link strip static
- [ ] All text pairs pass 4.5:1; verify with a contrast checker, not by eye
- [ ] Icon-only controls have accessible labels

**Design integrity**
- [ ] `npx playwright test visual-check` passes, including all "design system invariants"
- [ ] `document.fonts.check()` confirms all four faces loaded — no `system-ui` fallback
- [ ] Box-drawing glyphs (`├ └ ─ │`) render in the terminal, not tofu
- [ ] Verified at 100%, 125%, and 150% display scaling — hairlines and the 3px env tie survive
- [ ] Zero hex literals outside `tokens.css` (the ESLint rule enforces this)
- [ ] All numerics in Plex Mono with tabular-nums
- [ ] Cyan is used only for link state, never for actions
- [ ] Environment colour appears only as the row edge bar
- [ ] Broadcast mode and prod-context destructive actions are unmistakable
- [ ] AI content is violet-marked everywhere it appears

**Safety**
- [ ] Broadcast mode requires explicit activation and shows a persistent indicator
- [ ] Destructive prod actions require typing the hostname
- [ ] Read-only SFTP identities disable rather than hide mutating controls
- [ ] No secret value is ever rendered in plaintext without an explicit reveal action
- [ ] AI redaction preview appears before the first request of every session

---

## Appendix — Working with Claude Code on this

- **One step per session.** These steps are sized so each fits comfortably in context. Steps 8 and 12 are the largest; give them their own sessions.
- **Paste the relevant guide section into the prompt.** Don't rely on Claude Code having read the whole file — quote §0.6 when building `HostRow`.
- **Ask for the benchmark first** on anything performance-critical. "Write the Criterion benchmark for terminal keystroke latency, run it, report the number" — then optimize against a real baseline.
- **Reject the first table.** The instinct is a generic dashboard table; the spec asks for something denser and more specific. Say what's wrong with reference to the token system.
- **Keep a `docs/DECISIONS.md`.** When you deviate from this guide, record what and why. Future sessions read it and stop re-litigating settled choices.
