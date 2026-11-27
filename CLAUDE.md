# CLAUDE.md — OpsPilot

Unified engineering workspace: SSH, SFTP, Docker, Kubernetes, databases, REST/gRPC, tunnels, scripts, secrets. One desktop app, offline-first, config-file storage, plugin architecture, optional AI.

**The product thesis is speed.** Termius, MobaXterm, WinSCP and PuTTY all lose on latency and throughput. If a feature ships slow, it hasn't shipped.

**There is no backend.** The Rust core is a local process. Connections go straight from the user's machine to their infrastructure. Free and Pro require zero server infrastructure; only Enterprise (shared vault, SSO, RBAC) needs one.

---

## 1. Where the 10× actually comes from

Be honest about magnitudes. A long list of micro-optimizations does not produce an order of magnitude — a small number of architectural decisions do. Build these first; everything else is rounding.

| Path | The decision that wins | Realistic gain | What the competition does |
|---|---|---|---|
| **Perceived keystroke latency over WAN** | **Predictive local echo** (§5.5) | **20–40×** | PuTTY/Termius pay a full RTT before you see your own character |
| **Terminal output throughput** | Coalesced render + SoA grid + style interning (§5) | **10–50×** | PuTTY redraws per write with CPU-side GDI blits |
| **SFTP, one large file, high RTT** | Pipelined AIMD request window (§6.1) | **3–5×** | WinSCP: one outstanding request, one RTT each |
| **SFTP, thousands of small files** | **tar-pipe over an exec channel** (§6.2) | **30–100×** | WinSCP: ~4 round trips *per file* |
| **Connect time, perceived** | Speculative connect on hover/highlight (§8.2) | feels instant | Everyone waits for the user to commit first |
| **New session on a live host** | SSH channel multiplexing (§8.1) | **40×** | Termius performs a full re-handshake |
| **Kubernetes list/watch** | Protobuf + WatchList + informer (§10) | **5–15×** | Lens polls the API with JSON |
| **SQL grid, 1 M rows** | Arrow columnar + DataFusion (§11) | **5–20×** | DBeaver materializes row objects on the JVM heap |
| **Log search, 1 GB** | mmap + trigram/roaring + parallel scan (§9) | **50–500×** | Full rescan per query |
| **Whole-binary baseline** | mimalloc + rapidhash + LTO + PGO + BOLT (§14) | **1.4–2×** | stock release build |

Note the shape of this: the biggest single number on the list (tar-pipe) is not a clever algorithm, it is *noticing that the protocol is the wrong tool* and switching. Look for that pattern everywhere.

**The one sentence that matters most:** the UI thread does no I/O, no parsing, no hashing, and no allocation in the hot path — ever.

---

## 2. Why the competition is slow

| Tool | Root cause | Our counter-measure |
|---|---|---|
| PuTTY | Redraws on **every** PTY write; CPU-side GDI; whole-viewport invalidation; no echo prediction | §5.4 split render paths, §5.5 prediction, GPU atlas |
| WinSCP | **Synchronous single-outstanding-request SFTP** on the UI thread; `stat()` per file; 32 KB reads | §6 pipelining + tar-pipe + all I/O off the UI thread |
| MobaXterm | Cygwin POSIX emulation on every syscall | Native `io_uring` / IOCP |
| Termius | Electron: JS terminal renderer, per-keystroke GC, ~400 MB RSS, re-handshake per tab | Rust core + GPU renderer + channel multiplexing |
| Lens | Polls the K8s API with JSON, re-renders whole lists | Informer + protobuf + keyed reconciliation |
| DBeaver | JDBC materializes the full result set on the heap | Cursor streaming + Arrow columnar |

---

## 3. Stack

```
┌───────────────────────────────────────────────────────────────┐
│ Shell: Tauri v2 — webview ONLY for settings, dialogs, forms    │
│ Terminal / SFTP panes / large tables: wgpu surface, not webview │
├───────────────────────────────────────────────────────────────┤
│ Core: Rust 2021                                                │
│  · tokio (many-connection I/O)                                 │
│  · DEDICATED OS THREAD for the terminal hot path (§13.2)        │
│  · russh · bollard · kube-rs · sqlx/tiberius                   │
│  · arrow-rs + datafusion · regex (lazy DFA) · BLAKE3 · xxh3    │
│  · mimalloc · rkyv · memmap2 · roaring · fst · crossbeam       │
├───────────────────────────────────────────────────────────────┤
│ Storage: TOML config files (source of truth) + mmap'd index     │
│ IPC to webview: shared-memory ring + rkyv. Never JSON.          │
└───────────────────────────────────────────────────────────────┘
```

Hard rules:
- **No Electron. No JS in any hot path.** Anything updating faster than 4 Hz is a wgpu surface.
- Release profile is fixed: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `opt-level = 3`. PGO + BOLT in CI (§14.4).
- Engine crates behind feature flags and lazy-loaded. The Oracle driver must not be resident until someone opens an Oracle connection.
- No `unwrap()` outside tests. No blocking `std::fs`/`std::net` inside a tokio task.

---

## 4. Performance budgets (CI-enforced)

Asserted by Criterion with a regression gate. A PR regressing any row by >10% fails.

| Metric | Budget | Competitor |
|---|---|---|
| Cold start to interactive | **< 250 ms** | Termius ~2.8 s |
| Keystroke → glyph, **LAN** | **< 5 ms P50, < 12 ms P99** | PuTTY ~25 ms |
| Keystroke → glyph, **200 ms RTT WAN** | **< 8 ms** (prediction) | PuTTY ~205 ms |
| New tab on a connected host | **< 20 ms** | Termius ~900 ms |
| Cold SSH connect | < 350 ms; **~0 ms perceived** with speculative connect | — |
| `cat` a 200 MB file | sustained 60 fps, **zero dropped bytes** | PuTTY freezes |
| SFTP 1 large file, 1 GbE | **≥ 90% line rate** (~112 MB/s) | WinSCP ~28 MB/s |
| SFTP 10,000 × 4 KB files | **≥ 30×** WinSCP (tar-pipe path) | WinSCP ~9 min |
| Open 1 GB log, first paint | **< 120 ms** | — |
| Regex search, 1 GB log, warm index | **< 80 ms** | — |
| Filter 10,000-host tree per keystroke | **< 3 ms** | — |
| K8s 5,000 pods, list repaint | < 16 ms | Lens ~1.2 s |
| SQL grid 1 M rows: scroll / sort | 60 fps / **< 150 ms** | DBeaver OOM |
| Idle RSS, 5 live sessions | **< 120 MB** | Termius ~410 MB |
| Marginal RSS per extra terminal | < 40 MB | — |

Instrument with `tracing` + `tracing-flame`. Ship a hidden `Ctrl+Shift+P` overlay: frame time, PTY bytes/s, outstanding SFTP requests, prediction hit rate, RTT. **You cannot fix what you cannot see, and you cannot claim a 10× you did not measure.**

---

## 5. Terminal engine

### 5.1 Grid: structure-of-arrays, not array-of-structs

The renderer only ever needs *some* of a cell's attributes per pass. Splitting the grid into parallel typed arrays means each pass streams contiguous memory with a perfect SIMD stride, and an unchanged attribute plane is never touched.

```rust
struct Grid {
    cols: usize,
    glyph: Vec<u32>,    // ASCII codepoint, or interned GraphemeId (high bit set)
    style: Vec<u16>,    // index into a per-screen StyleTable
    // idx = row * cols + col
}

// Most screens use < 256 distinct (fg, bg, flags) combinations.
// Interning them cuts the per-cell footprint from 16 bytes to 6.
struct StyleTable { entries: Vec<Style>, lookup: HashMap<Style, u16> }
struct Style { fg: u32, bg: u32, flags: u16 }
```

- **6 bytes/cell instead of 16.** A 200×50 viewport is 60 KB — fits in L2. Memory bandwidth on the GPU upload path drops ~2.6×.
- **Never `Vec<Vec<Cell>>`.** That is a pointer chase per row and a real cause of missed frames.
- Flat indexing only: `idx = row * cols + col`.
- Wide (CJK/emoji) cells occupy two slots; the second carries `WIDE_SPACER`.

### 5.2 VT parsing: table-driven DFA + SIMD fast path

Implement Paul Williams' VT500 parser as a **static transition table** `[[State; 256]; N_STATES]` — one indexed load and one branch per byte, no string matching, no allocation.

- **ASCII bulk path**: before entering the DFA, use `memchr` to find the next byte `< 0x20 || == 0x7F`. Everything before it is printable ASCII — bulk-write glyph and style planes in a tight vectorized loop. Real terminal output is >95% this path, and this is where most of the throughput multiple lives.
- UTF-8 validation via `simdutf8::basic::from_utf8` (~10 GB/s). Grapheme clustering only off the ASCII path.
- Parser scratch state uses a **bump arena** (`bumpalo`) reset each frame. Zero allocator traffic in steady state.

### 5.3 Damage tracking

`dirty_rows: [u64; ceil(rows/64)]` bitset. Mark is `dirty[r >> 6] |= 1 << (r & 63)` — O(1), branchless. The renderer scans words and skips zero words via `trailing_zeros()`. Only changed rows upload. Never upload the whole grid unless the viewport scrolled or resized.

For grids over ~4,000 cells wide, escalate to **dirty rectangles** and pass them to the GPU as scissor rects.

### 5.4 Two render paths (this is the PuTTY fix, refined)

My earlier draft coalesced everything to vsync. That is wrong for input: it adds up to 16 ms to every keystroke. **Split the paths.**

```
Keystroke ────────────────────────────▶ predict + render IMMEDIATELY (no vsync wait)
PTY output ──(lock-free SPSC ring)──▶ parser ──▶ grid ──▶ coalesced at vsync
```

- **Input path: render on the input event.** Latency matters more than throughput; one extra draw call per keystroke is free.
- **Output path: coalesced, once per vsync**, regardless of how many writes arrived. 10,000 writes in one frame = one draw.
- **Backpressure rule: drop frames, never drop bytes.** All bytes are parsed into the grid; only intermediate *visual* states are skipped. Scrollback stays correct.
- If the parser falls >2 frames behind, enter **flood mode**: parse without emitting damage, render only the final state.
- Triple-buffer the GPU staging buffer so the CPU never stalls waiting on the previous present.

### 5.5 Predictive local echo — the largest single win

Over a 200 ms link, every competitor makes you wait 200 ms to see your own keystroke. Don't. Predict the echo, render it instantly, reconcile when the server replies. This is Mosh's technique and it is the difference between "usable" and "remote".

**Design — two grids, one epoch counter:**

```
authoritative_grid   ← only ever mutated by bytes from the server
predicted_grid       ← authoritative + locally predicted edits, THIS is what renders
predictions: VecDeque<Prediction>   // { epoch, cursor, bytes_sent, applied_edit }
```

1. On keystroke: send to the server **and** apply the predicted edit to `predicted_grid`. Render immediately.
2. On server bytes: apply to `authoritative_grid`. Pop the oldest matching prediction and compare.
3. **Match** → confirm, drop the prediction, keep rendering predicted.
4. **Mismatch** → discard *all* outstanding predictions and snap `predicted_grid = authoritative_grid`. Never try to partially repair; a hard snap is correct and cheap.

**Only predict when confident.** Getting this wrong is worse than not predicting, so gate it hard:

- Predict only printable ASCII, `Backspace`, and horizontal cursor movement.
- Predict only when the cursor is at or near end-of-line in a normal (non-alternate-screen) buffer.
- **Never predict** when: the alternate screen buffer is active (vim, htop, less), bracketed paste is in flight, the server has echo disabled (password prompts — detect via the absence of echo on prior input), an OSC/DCS sequence is open, or the prediction queue is deeper than ~12 unconfirmed edits.
- Adaptive: track a rolling **prediction hit rate**. Below ~85%, disable prediction for that session and log it. A wrong guess that flickers is a bug the user will feel.
- Optional setting: underline unconfirmed predictions (Mosh does this). Default off, but valuable for debugging and for users on very bad links.

Measure and expose the hit rate in the perf overlay. **If prediction hit rate is not instrumented, prediction is not shippable.**

### 5.6 Scrollback and reflow

- **Ring buffer** of rows, fixed capacity (default 100k). Append is O(1), oldest overwritten in place. Never `Vec::remove(0)` — that is O(n) per line and exactly why some emulators stutter under `yes`.
- Variable-length rows: trailing blanks are not stored. A 5-char line in an 800-col terminal costs bytes, not 800 cells.
- **Reflow on resize**: reflow the visible viewport synchronously (bounded, ~50 rows), then reflow scrollback lazily on scroll in 1,000-row chunks on a background task. Resizing with 100k rows must never block for seconds.

### 5.7 Rendering

- Glyph atlas: one `2048×2048` R8 texture. Pre-rasterize ASCII `0x20–0x7E` at startup for the configured font/size; **LRU cache** for the rest, repack on pressure.
- Instanced quads in a **persistent mapped buffer**, one draw call per frame. Subpixel-position cache keyed by `(glyph, subpixel_bucket)`.
- Cursor blink is a separate 1-quad overlay pass — it must never dirty the screen.
- Ligatures **off** in the terminal. `!=` must not render as `≠` in a shell.

### 5.8 Search

| Case | Algorithm | Why |
|---|---|---|
| Literal substring | `memchr::memmem` (SIMD two-way) | ~5 GB/s, sublinear skips |
| Multi-keyword highlight | **Aho-Corasick** | All patterns in one pass |
| Regex | Rust `regex` (**lazy DFA**) | Linear time, **no catastrophic backtracking** |
| Row rejection prefilter | **Per-row 256-bit bigram bitmap** (`[u64;4]`) | Reject ~99% of rows with 4 `AND`s |
| Persistent index | **Trigram → roaring bitmaps** (Zoekt design) | Candidate set in microseconds |

### 5.9 Windows-specific warning

**ConPTY is a real throughput ceiling** and it is not your bug to fix in Rust. It reserializes output through a hidden console host, and it will cap you well below the Linux path. Budget for it: measure the Windows terminal path separately, set a separate (looser) budget row, and pass `PSEUDOCONSOLE_INHERIT_CURSOR`. Do not spend weeks assuming your parser is the problem.

---

## 6. SFTP engine — the flagship win

### 6.1 Pipelined AIMD transfer window

WinSCP sends one `SSH_FXP_READ`, waits a full RTT, sends the next. On a 40 ms link that caps at ~800 KB/s regardless of bandwidth.

- Treat outstanding requests as a **congestion window**. Start at 16 × 256 KB. **AIMD**: window of ACKs with no stall → `w += 1`; timeout or buffer-full → `w /= 2`. Clamp `[4, 128]`.
- Target `window_bytes ≈ measured_bandwidth × RTT × 1.3` (bandwidth-delay product).
- Reassemble out-of-order responses in a small `BTreeMap<offset, Bytes>`; flush contiguous prefixes.
- **Widen the SSH channel window too.** `russh` defaults are small (~2 MB or less); a narrow channel window stalls you no matter how deep the SFTP pipeline is. Set it to ≥ 8 MB and adjust proactively, not lazily.

### 6.2 The small-files problem — switch protocols, don't tune

Per-file SFTP costs ~4 round trips (`OPEN`/`WRITE`/`CLOSE`/`SETSTAT`). 10,000 files × 4 × 40 ms = 26 minutes of pure waiting. No window size fixes this.

- **Detect**: queue has >200 files averaging <1 MB.
- **Tar-pipe**: open an `exec` channel, stream `tar -cf - -C dir .` / `tar -xf - -C dir`. One round trip total. 30–100× faster. Fall back to SFTP if `tar` is missing or `exec` is disallowed.
- Otherwise **4–8 parallel channels** on the same TCP connection, files distributed by a **work-stealing deque** (`crossbeam-deque`) so one 500 MB file doesn't starve a worker.
- **Surface the switch in the UI** ("batched · 8,412 files"). A user who has waited nine minutes in WinSCP deserves to know why this took eleven seconds.

### 6.3 Zero-copy and syscall efficiency

- **Linux**: `io_uring` with **registered fixed buffers** (`IORING_REGISTER_BUFFERS`), registered files, `IOSQE_IO_LINK` for read→write chains, and `SENDMSG_ZC` for zero-copy send. Consider `IORING_SETUP_SQPOLL` for the transfer path only — it burns a core, so gate it behind a "max throughput" setting.
- **Windows**: IOCP with `SetFileCompletionNotificationModes(FILE_SKIP_COMPLETION_PORT_ON_SUCCESS)` to skip a completion packet on synchronous success; `TransmitFile` for disk→socket.
- **macOS**: `kqueue` + `sendfile`.
- Buffer pool of pre-allocated 256 KB slabs, recycled. **Zero allocations per chunk** in steady state.
- **`TCP_NODELAY` on every socket.** Nagle's algorithm adds up to 40 ms to interactive traffic. This is one line and it is not optional.

### 6.4 Cipher selection

- Runtime-detect AES-NI (`is_x86_feature_detected!("aes")`). Prefer `aes128-gcm@openssh.com` where present, `chacha20-poly1305@openssh.com` otherwise. **Wrong cipher choice alone is a 3× throughput difference.**
- SSH compression **off by default**. Measure goodput; enable only below ~2 MB/s with idle CPU. Compressing on a LAN makes transfers slower.

### 6.5 Compare and sync

- **Merkle tree** of the directory: hash files, hash directories from children. Comparing two trees costs O(differences), not O(files) — unchanged subtrees prune at the top node.
- **BLAKE3** for content hashes (SIMD + rayon, ~7 GB/s). **`xxh3`** for non-cryptographic cache keys — much faster and cryptographic strength is pointless there.
- Cache `(path, size, mtime, ino) → blake3`; re-hash only when the stat tuple changes.
- Delta transfer: **rsync rolling checksum** (Adler-32 weak window to find boundaries, BLAKE3 to confirm). Send only differing blocks.
- Resume: persist `(offset, blake3_of_prefix)` and **verify the prefix hash before resuming**, so a truncated file is never silently corrupted.

### 6.6 Listing and prefetch

- `readdir` in batches. **Never `stat` per entry** — that is the WinSCP "loading…" spinner.
- **Speculative prefetch**: when a directory opens, prefetch listings for its immediate subdirectories on an idle-priority task. Navigation then feels instant. Cancel on navigate-away.
- Virtualized panes; sort with `pdqsort` over a pre-extracted key array, never over row structs.

### 6.7 Optional accelerator: helper agent (post-1.0)

If a user installs a small OpsPilot agent on the server, you can bypass SSH's framing entirely: QUIC with 0-RTT resumption, multipath, BBR, and native delta sync. This is the only route to beating SFTP's structural limits rather than working around them. **Design for it, ship it later, never require it** — "works with stock sshd" is a core promise.

---

## 7. Host tree and search

Target: 10,000 hosts, keystroke → repaint under 3 ms.

- **Fuzzy match**: fzf v2 — bit-parallel prefilter (`u64` per pattern char) rejects non-candidates in one pass, then Smith-Waterman scoring on survivors only. Cache scores keyed by `(host_id, query_prefix)` so each extra character reuses prior work.
- **Prefix autocomplete**: **FST** (`fst` crate) with a Levenshtein automaton for typo tolerance. Memory-mapped, ~1 MB per 100k entries.
- **Tag / env / region filters**: inverted index `tag → RoaringBitmap<host_id>`. Multi-tag filter is a bitmap `AND` — microseconds.
- **String interning** for hostnames, paths, tags (`lasso` or an arena interner). Comparisons become `u32` equality and the index shrinks.
- Flatten the tree to `Vec<VisibleNode>` on expand/collapse; virtualize rendering. **Never recurse during paint.**
- Query runs on a background task with a **cancellation token**; each keystroke cancels the in-flight query. Debounce 16 ms.

---

## 8. Connection layer

### 8.1 One transport, many channels

- **One SSH transport per host; SSH sessions, SFTP profiles, and port forwards multiplex as channels over it.** This is what makes a new tab cost 20 ms while Termius re-handshakes for 900 ms.
- **But**: the PRD requires **multiple SFTP identities per host**, and different usernames/keys need *separate* transports. Model as `HostId → Vec<Transport>` keyed by `(user, auth_material, jump_path, proxy)`. Channels multiplex within a transport; identities do not share one.
- **Actor model**: one tokio task per transport, owning its state, reachable only via an `mpsc` mailbox. No `Mutex<SshSession>` — therefore no lock contention and no deadlocks.
- **LRU pool** (intrusive list + `HashMap`), idle-evicted on TTL. Pinned hosts stay pre-warmed.

### 8.2 Speculative connect

Connect time you spend before the user commits is connect time they never experience.

- Begin TCP + SSH handshake when a host is **highlighted in the command palette** or hovered in the tree for >180 ms.
- Cap concurrent speculative connects (~3) and cancel on navigate-away. Tear down unused ones after ~10 s.
- **Never speculatively connect** when auth would prompt the user (password, MFA, hardware token, or a locked vault) — a surprise touch-prompt is a bug. Silent key/agent auth only.
- Respect a per-host opt-out; some bastions rate-limit or alert on connection attempts, and an audit-logged connection the user did not intend is a serious problem.

### 8.3 Reliability

- Reconnect with **exponential backoff and full jitter**: `min(cap, base * 2^n) * rand(0..1)`. A fixed interval thundering-herds a bastion.
- Adaptive keepalive: start 30 s, halve on detected NAT timeout.
- Tunnel health checks on **one shared timer wheel**, not one timer per tunnel.

---

## 9. Log viewer and large files

- **`mmap` the file.** Never `read_to_string`. First paint touches only the pages you paint.
- **Sparse line index**: byte offset of every 1,024th line in a `Vec<u64>`; scan between anchors on demand. A 50 M-line file costs ~390 KB. **Elias-Fano** or delta+varint if it must shrink further.
- **Parallel first scan**: split the mmap into per-core chunks with `rayon`, aligning chunk boundaries to the next newline so no line is split. Index-build becomes core-count-parallel.
- Time filtering: timestamps are monotonic → **binary search** the line index. O(log n), never a full scan.
- Tailing: `notify` (inotify / `ReadDirectoryChangesW`), incremental parse from the last offset. Handle truncation and rotation by watching the inode/file-id.
- JSON logs: **`simd-json` on-demand parsing** — extract only projected columns, never build a DOM for a line you render as one row.
- ANSI colouring reuses the terminal DFA parser. One implementation, not two.

---

## 10. Kubernetes and Docker

- **Protobuf, not JSON.** Set `Accept: application/vnd.kubernetes.protobuf`. 2–3× faster decode and much less allocation. Single biggest kube-client win and it is a header.
- **Watch, don't poll.** Informer pattern: `List` then `Watch` from that `resourceVersion`; relist on `410 Gone`. Lens polls; that is why Lens is slow.
- **WatchList bootstrap** (K8s ≥ 1.27): `sendInitialEvents=true` with `resourceVersionMatch=NotOlderThan` streams the initial state as watch events instead of one enormous LIST response. Removes the startup spike on large clusters. Feature-detect and fall back to List+Watch.
- Client-side **store + secondary indexes**: `HashMap<Uid, Resource>` plus `by_namespace`/`by_label`/`by_node` as roaring bitmaps. Filtering never scans.
- **DeltaFIFO** between watch stream and UI, deduplicating by UID — a churning pod produces one repaint, not fifty.
- **Token-bucket rate limiter** on the work queue so a 5,000-pod resync can't saturate a frame.
- Repaint by **keyed reconciliation**: diff by UID, patch changed rows. Never rebuild the table.
- YAML parsed documents cached by content hash.
- Docker: **events API + streaming stats**, never polling. Log streaming reuses the terminal ring buffer and coalescing rules verbatim.

---

## 11. Database module

- **Cursor streaming.** Fetch N rows (default 500), render, prefetch the next chunk past 70% scroll. Never materialize the result set.
- **Arrow columnar buffer** (`arrow-rs`): one typed contiguous array per column. This is what makes the rest cheap.
- **DataFusion** for client-side filter, sort, and aggregate over those Arrow batches. You get a vectorized, parallel query engine for free instead of hand-writing grid operations. Sorting 1 M rows becomes a **radix sort over a primitive array**, not a comparison sort over boxed structs.
- **Postgres**: use the binary format and **extended-query pipelining** — batch parse/bind/execute without waiting per statement. Large exports via `COPY ... TO STDOUT BINARY`.
- LRU prepared-statement cache per connection. Schema cache on disk; autocomplete via FST + fuzzy, same code path as the host tree (§7).
- Export streams chunk-by-chunk to disk. A CSV export must never materialize in RAM.

---

## 12. Storage: config files are the source of truth

**Replaces the earlier embedded-database design.** An embedded DB is an opaque binary blob; a user cannot copy it to another PC with confidence, diff it, or put it in git. Config files can. `redb` survives only as a *derived cache* (§12.4), never as truth.

### 12.1 Two directories, one portable

```
~/.config/opspilot/                  ← PORTABLE. Copy this and it works.
  opspilot.toml                      schema_version, active workspace
  settings.toml                      fonts, theme, terminal, transfer tuning
  keybindings.toml
  workspaces/
    production.toml                  hosts + their SSH/SFTP/DB/K8s connections
    development.toml
    customer-a.toml
  identities.toml                    identities → vault refs. NO secrets.
  tunnels.toml  snippets.toml  databases.toml
  api/collections/*.toml
  themes/*.toml
  known_hosts                        portable — host keys belong to servers
  vault.bin                          encrypted; portable (see §12.3)
  plugins.toml                       names + versions, not binaries

<OS state dir>/opspilot/             ← DO NOT COPY. All rebuildable.
  machine.toml                       window geometry, panel widths, monitors
  session.json                       tab restore state
  cache/index.rkyv                   FST + roaring bitmaps, derived
  cache/hashes.redb                  xxh3/BLAKE3 stat cache
  logs/
```

`OPSPILOT_CONFIG_DIR` overrides the portable root, so a fully portable USB install is just an env var.

### 12.2 Format rules

- **TOML, not YAML.** YAML will silently coerce a port value of `no` into `false`. That class of bug does not belong in a tool that connects to production.
- **No absolute paths in portable config — ever.** This is the single most common thing that breaks an exported config. Use `~/...`, `${vault:prod/deploy-key}`, or `${env:VAR}`; always forward slashes, resolved per-platform at load. Add a CI lint that rejects any drive letter or `/home/<user>` literal in a serialized fixture.
- **Atomic writes always**: temp file → `fsync` → rename. Never rewrite a workspace file in place. A crash mid-write must not cost the user their host list.
- `schema_version` in `opspilot.toml`, with forward migrations that back up the original first. Never silently rewrite a config from a newer version than the binary understands — refuse and say so.
- **Hot reload**: watch the config dir with `notify`. Editing `production.toml` in Vim makes hosts appear in the tree. Termius makes you click through dialogs; this is a real differentiator and it costs almost nothing.

### 12.3 Vault portability — get this right or there is no recovery

- Vault is **AES-256-GCM**, per-record nonce, root key from **Argon2id** (m=64 MiB, t=3, p=4) over the master password.
- **The Argon2 params and salt live inside `vault.bin`.** The root of trust is the master password, nothing else.
- The OS keychain (Windows Credential Manager/DPAPI, macOS Keychain, Secret Service) stores only a **wrapped copy** of the derived key, as a local biometric convenience. On a new PC that wrap is simply absent → fall back to the password → re-wrap locally.
- **If you make DPAPI the root of trust instead, the vault becomes permanently unopenable on any other machine, with no recovery path.** This is a one-line design decision with permanent consequences.
- Keys live in `zeroize`d, `mlock`ed buffers. Never a `String`, never logged, never in a panic message. Write manual `Debug` impls printing `<redacted>`.
- Secrets broker in a **separate process**; the UI receives a handle, not plaintext, so a webview XSS cannot read the vault.
- Audit log append-only and hash-chained: `h[n] = blake3(h[n-1] || entry)`.

### 12.4 Keeping config files fast

Parsing 10,000 hosts and rebuilding indexes on every launch would blow the 250 ms budget. Don't.

1. Compute a validity token from every config file's `(size, mtime)`, hashed with `xxh3`.
2. Token matches `cache/index.rkyv` → **`mmap` the prebuilt index and use it in place**. `rkyv` needs no deserialization pass, so this is effectively free.
3. Token differs → reparse (~50 ms for 10k hosts), rebuild FST + roaring bitmaps, write the cache atomically.

The cache is never exported, so a fresh PC pays that one-time cost on first launch and is fast forever after. **Truth is human-readable and portable; speed is derived and disposable.**

### 12.5 Sync without a backend

Do not build a sync server. The config dir is a plain folder — point it at a private git repo, S3, Syncthing, or Dropbox. Ship `opspilot config export|import|diff` and a git-friendly `.gitignore`. You get the Pro sync feature, users keep credentials off your infrastructure, and you operate nothing.

Pro licensing likewise needs no server: **Ed25519-signed offline license keys** with the public key embedded in the binary. Verification is local and works on a plane.

---

## 13. Concurrency

### 13.1 Rules

1. UI thread: no I/O, no parsing, no hashing, no unbounded `.await`. If a handler can exceed 2 ms, move it.
2. One actor task per connection. Communicate by channel, not shared memory.
3. PTY byte streams use **lock-free SPSC ring buffers** (`rtrb`), not `Mutex<Vec<u8>>`.
4. Read-mostly config: `arc-swap`. Short contended sections: `parking_lot`. Never `std::sync::Mutex` in a hot path.
5. CPU-bound fan-out (hashing, compare, sort, index build): `rayon`. Never on the tokio runtime.
6. Every long-running task takes a `CancellationToken`. Closing a tab cancels its work immediately.
7. **All channels bounded.** An unbounded channel is a memory leak with extra steps.
8. Progress via `AtomicU64` sampled by the UI at 10 Hz. Never one message per chunk — at 100 MB/s with 256 KB chunks that is 400 msg/s and it will melt the frame budget.

### 13.2 Keep the terminal hot path off tokio

tokio is the right tool for hundreds of concurrent connections. It is the wrong tool for one latency-critical byte stream: task wake-up costs microseconds and the scheduler can delay you unpredictably.

**Give the focused terminal's PTY-read → parse → damage path a dedicated OS thread** at elevated priority, with a spin-then-park loop on the SPSC ring. Background/unfocused sessions stay on tokio. This is worth single-digit milliseconds of P99 keystroke latency — which is a large fraction of a 12 ms budget.

---

## 14. Systems-level tuning

Individually small; together roughly 1.4–2× across the board. Do these once, early, and stop thinking about them.

### 14.1 Allocator
Global **`mimalloc`**. 10–30% on allocation-heavy paths and much better multithreaded behaviour than the system allocator (especially msvcrt on Windows). Combine with **bump arenas** (`bumpalo`) for per-frame parser scratch and `smallvec`/`smallstr` to keep short strings off the heap.

### 14.2 Hashing
Replace SipHash. **`hashbrown` + `rapidhash`/`foldhash`** for internal maps — DoS resistance is irrelevant for local data structures and SipHash is pure overhead. `xxh3` for cache keys and validity tokens. BLAKE3 only where content-addressing genuinely needs cryptographic strength.

### 14.3 SIMD dispatch
Compile hot kernels (cell blitting, bigram prefilter, hex/base64) for multiple targets and dispatch at runtime via the `multiversion` crate, so AVX2/AVX-512/NEON machines get the fast path without a separate build. Prefer `std::simd` where stable, `memchr`/`simdutf8` where they already solved it.

### 14.4 Build pipeline
`lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, then:
- **PGO**: instrument, run a scripted workload (open sessions, `cat` a large file, transfer files, filter hosts), rebuild with the profile. 10–15% typical.
- **BOLT** post-link layout optimization on top of PGO. Another 5–10% on a large binary, mostly icache.
- Dev builds use `mold`/`lld` for link speed. **This does not go in release builds** — it is developer velocity, not runtime speed.

### 14.5 Cold start
- Lazy-load engine crates. Nothing K8s-, Oracle-, or plugin-related is resident until first use.
- `mmap` the prebuilt index (§12.4) instead of parsing.
- Restore window layout immediately; reconnect terminals lazily after first paint.
- Audit dependency count against binary size. Cold start is a 250 ms budget and every crate spends some of it.

### 14.6 Small things with outsized effect
- **`TCP_NODELAY` on every socket.** Up to 40 ms of interactive latency, one line.
- Widen SSH channel windows (§6.1).
- Match the terminal's render loop to the display refresh rate; don't hardcode 60 Hz on a 144 Hz panel.
- Smooth rate readouts with a 1 s EWMA and `tabular-nums`. A number jittering between 94 and 108 MB/s reads as broken even when it isn't.

---

## 15. Optional AI

Enhancement, never a dependency. Every core feature works with AI fully disabled — the PRD is explicit, and it keeps operating cost at zero.

- Providers: Ollama (local), or the user's own Anthropic/OpenAI/Gemini key.
- All calls async, cancellable, off the UI thread. Never block a terminal on inference.
- **Redact before sending.** Strip secret-pattern matches (keys, tokens, optionally IPs) from prompts and show the user exactly what will be transmitted.
- Suggested commands are **never auto-executed** — insert into the prompt line for explicit confirmation.

---

## 16. Working conventions for Claude Code

- **Measure before optimizing.** Add a Criterion benchmark, record the number, then change the code. A PR claiming a speedup without a bench delta is not reviewable.
- **Name the budget.** Which row of §4 does this feature touch, and what did it cost?
- **Check §1 before reaching for micro-optimizations.** If a path is slow, ask whether the *protocol or architecture* is wrong before tuning the loop. tar-pipe beat every possible SFTP tuning by 100×.
- Prefer a flat array over a pointer tree; an index over a scan; a bitset over `HashSet<u32>`; borrowing over cloning.
- State the complexity of any loop over user data in a comment. O(n²) or worse must be justified or replaced.
- No new dependency without a note on binary size and cold-start impact.
- Tests:
  - unit tests per module;
  - `tests/vt_conformance.rs` against `esctest`/`vttest` vectors;
  - `proptest` for the parser, reflow, **and prediction reconciliation** — property: for any input sequence, `predicted_grid` converges to `authoritative_grid`;
  - SFTP integration against a Dockerised `openssh-server` with **`tc netem` injecting 40 ms RTT and 1% loss**. Naive code passes on localhost and fails in production; the pipelining wins only show up under latency;
  - a config **round-trip test**: export on Linux, import on Windows, assert byte-identical resolved state.
- Commit format: `<module>: <imperative summary>` — e.g. `sftp: widen pipeline window via AIMD`.

---

## 17. Build order

Ship the performance spine first. Everything else attaches to it.

1. **Terminal engine** — SoA grid, style interning, ring scrollback, DFA parser, wgpu renderer, split input/output render paths. **Prove the 5 ms keystroke and 60 fps `cat` budgets before writing any UI chrome.**
2. **SSH transport** + channel multiplexing + connection pool + `TCP_NODELAY` + wide channel windows.
3. **Predictive local echo** (§5.5) with hit-rate instrumentation. Do this early — it shapes the grid API, and retrofitting two-grid reconciliation later is painful.
4. **Config layer** (§12) — TOML schema, atomic writes, index cache, vault, hot reload. Everything downstream persists through this, so getting the portability rules right now avoids a migration later.
5. Host manager, workspace tree, fuzzy search.
6. **SFTP** — pipelining, tar-pipe, dual pane, Merkle compare, speculative prefetch.
7. Port forwarding manager.
8. Docker, then Kubernetes (protobuf + WatchList + informer).
9. Database module (Arrow + DataFusion), then REST/API client.
10. Log viewer, monitoring, scripts.
11. Plugin host (WASM, sandboxed, capability-based), then optional AI.

**Do not start step 6 until step 1's budgets are green in CI.** The entire product's credibility rests on them.

---

## 18. Notes from the build (added during implementation)

Things the original document did not say, that turned out to matter. Each was
found by building the thing, not by reasoning about it. Where one contradicts a
section above, the section above is the intent and this is the correction.

### 18.1 Library defaults fight §14.6 and §6.1

`russh`'s `client::Config` ships **`nodelay: false`**. §14.6 calls TCP_NODELAY
"one line and it is not optional" — that line is `nodelay: true`, and forgetting
it costs up to 40 ms per interactive round trip with no visible error. Its
default channel window is likewise small; §6.1 wants ≥ 8 MB. Both are set
explicitly in `crates/core-ssh/src/transport.rs`. **Audit every library default
against §4 rather than assuming the sensible value is the default.**

### 18.2 The grid must be a ring, not a memmove (amends §5.1)

§5.1 mandates flat SoA planes and forbids `Vec<Vec<Cell>>`, but says nothing
about scrolling, and the obvious reading — `copy_within` over the planes — is a
trap. Measured: the parser ran at 400 MiB/s standalone and **7.45 MiB/s** with
the grid attached, because a 4 MiB `cat` is ~75,000 line feeds each memmoving the
whole grid: 4.5 GB of memmove to display 4 MiB.

Logical row `r` now lives at physical row `(r + row_offset) % rows`. Scrolling
increments the offset and blanks one row. **+530%**, measured. Every §5.1
property survives: rows stay contiguous, no pointer chase, one add and one
predictable compare per lookup.

### 18.3 `memchr` is the wrong tool for the ASCII bulk path (amends §5.2)

§5.2 says "use `memchr` to find the next byte `< 0x20 || == 0x7F`". `memchr`
searches for *specific bytes*; this is a *range* predicate. It is implemented as
SWAR over `u64` words — `hasless(w, 0x20) | hasmore(w, 0x7E)` — measured at
**821 MiB/s**, and it keeps `core-term` free of runtime dependencies entirely.

### 18.4 C1 controls must be disabled in UTF-8 mode

The VT500 spec's "anywhere" transitions treat 0x80–0x9F as controls. In UTF-8
those are continuation bytes, so honouring them makes the second byte of `œ`
(0x9C) terminate a string and corrupt any non-ASCII output. Every modern terminal
disables C1 in UTF-8 mode. Likewise `:` must be a CSI parameter separator, not an
error, or `SGR 38:2::R:G:B` truecolour is discarded.

### 18.5 §5.5's two grids can be one grid plus an overlay

The gating rules in §5.5 permit only printable ASCII, backspace, and horizontal
cursor movement — each of which touches **at most one cell plus the cursor**. So
a small overlay list is complete: there is no predicted state a second grid could
represent that `(row, col, glyph, style)` cannot. The second grid would cost a
full copy per server read plus a duplicate scrollback. Semantics are unchanged,
including the hard snap on mismatch.

### 18.6 Host key verification is the security boundary, and its library default is "reject"

`russh`'s `Handler::check_server_key` defaults to rejecting everything. That is
the right default, and it means **a wrong implementation is a silent downgrade,
not a visible failure** — an accidental `Ok(true)` disables the entire protection
with no error anywhere. Keep it in its own module with its own tests.

The rule that does not bend: **a changed key is refused under every policy**,
including trust-on-first-use. A rotation and a machine-in-the-middle are
indistinguishable from the client, so there is no "connect anyway" affordance.

Two subtleties worth carrying forward:

- `ssh-key`'s `PublicKey` equality **includes the comment field**, and
  `learn_known_hosts_path` round-trips through `to_openssh()`. A key with a
  comment can therefore compare unequal to its own recorded copy and report a
  *changed host key*. Real server keys carry no comment.
- `known_hosts` keys on host **and port**: a key learned for `:22` must not
  authorise `:2222`.

### 18.7 Windows build reality

- **`aws-lc-sys` needs NASM** and fails outright without it. Use russh's `ring`
  backend: pregenerated assembly, no build toolchain. Both use hardware AES,
  which is what §6.4 actually cares about.
- **`tauri-build` needs `icons/icon.ico` to *compile*** on Windows, not merely to
  bundle — it emits a Windows Resource file during the build script, so
  `cargo check --workspace` fails without it.
- **A running exe locks itself.** `tauri build` fails with "Access is denied" if
  the app is open. Close it first.
- ConPTY is reached via `portable-pty`; **drop the slave handle after spawn** or
  the PTY never reports EOF and the reader thread hangs forever on a dead
  session.

### 18.8 Canvas `ctx.font` does not support `var()`

`ctx.font = "12.5px var(--font-term)"` is **silently ignored** — canvas parses a
CSS font shorthand with no element context, so the assignment fails and the
context keeps `10px sans-serif`. The terminal then renders in a proportional face
while cell width is measured from the same broken font, and the cursor lands
nowhere near the text. Resolve the family with `getComputedStyle` first.

Related: even in a monospace face, drawing a whole string lets sub-pixel advances
accumulate and characters drift out of their cells. Draw glyph by glyph at exact
cell origins.

### 18.9 §13.1 rule 8 applies to terminal output, hard

The rule is written about progress counters, but PTY output is the case that
really needs it: thousands of small writes per frame. The reader thread bumps an
`AtomicU64` and the UI samples once per animation frame. **Always include the
cursor row in a snapshot**, even when the damage list calls it clean — the shell
writes its prompt there, and a single dropped update makes the terminal look
hung until something unrelated forces a repaint.

### 18.10 Process-global state makes parallel tests lie

Tests that set `OPSPILOT_CONFIG_DIR` clobbered each other, because cargo runs
tests in parallel threads within one process. The symptom was a save-then-load
returning zero hosts — it looks like a storage bug and is a concurrency bug.
Serialise env-var-dependent tests behind a mutex.

### 18.11 Defaults that are safety decisions

- Port forwards bind **loopback**, never `0.0.0.0`. The other default exposes a
  tunnel into production to the whole local network.
- A SOCKS proxy must **refuse** BIND and UDP ASSOCIATE explicitly rather than
  accept and hang.
- On Windows the local file pane reports `?????????` for permissions rather than
  inventing `0644`. Windows has no mode bits, and fabricating plausible values in
  a tool used to manage production is worse than admitting ignorance.
- Directory transfers are **disabled** until the transfer queue exists, rather
  than being a button that appears to work.

### 18.12 What §12.3 leaves out about the vault format

§12.3 specifies the cryptography exactly — AES-256-GCM, per-record nonce,
Argon2id at m=64 MiB/t=3/p=4, params and salt inside the file — and that is the
hard part done. But the *container* has three failure modes the section does not
mention, and AES-GCM does not prevent any of them on its own.

**1. Ciphertexts can be moved between records.** GCM authenticates a message; it
does not authenticate *which record the message belongs to*. Copy the ciphertext
of `stage/root` over `prod/root` and the vault decrypts it happily and hands
back staging's password for a production connection. No error, anywhere.

The fix costs nothing: pass the record id as the GCM **associated data**. Now the
same ciphertext under a different id fails authentication.

**2. Records can be deleted by truncating the file.** Any count-prefixed format
has this hole — drop the last record, decrement the count, and it parses
cleanly. Worse, the next save makes the deletion permanent. Seal a hash of the
id list as a trailing record; the ids are not secret, so this leaks nothing and
detects the edit on open.

Length-prefix each id before hashing, or `{"ab","c"}` and `{"a","bc"}` hash
identically.

**3. A hostile header is an allocation bomb before it is anything else.** The
Argon2 parameters are read from the file, and §12.3 says they should be. An
`m_cost` of `u32::MAX` asks for a 4 TiB allocation during `open()`. Bound the
parameters before deriving.

What §12.3 *does* get for free, and is worth noticing so no one adds a redundant
header MAC: the header feeds the KDF, so a tampered salt or cost parameter
produces a key that cannot decrypt the verifier record. The header is
authenticated by consequence.

### 18.13 A hash chain makes tampering visible, not impossible

§12.3 asks for the audit log to be "append-only and hash-chained:
`h[n] = blake3(h[n-1] || entry)`". Worth being precise about what that buys,
because it is easy to overstate in a UI:

- Editing or removing an entry in the middle **breaks verification** at that
  point. This is the real guarantee.
- Truncating the **tail** leaves a chain that still verifies perfectly. A hash
  chain with no external anchor cannot detect its own tail being cut.

Anyone who can write the log file can rewrite it from any point forward. So show
the break position when verification fails, and do not describe the log as
tamper-proof. The test suite should assert the truncation limit explicitly
rather than quietly not covering it.

One implementation detail with a security edge: **sanitise the id before writing
the line**. Record ids come from the user, and a newline in one forges log
entries directly.

### 18.14 The command list is a security boundary, so test it as one

§12.3 puts the secrets broker in a separate process. If that is not available —
Tauri v2 has no supported multi-process split — the property to preserve is the
one the broker existed for: **no command returns a secret**. Authenticate by
reference (`vault:prod/deploy`) and resolve it natively.

That property is invisible in review and easy to lose to a single well-meaning
"add a reveal button" commit. It is also trivially testable: read the
`generate_handler!` list with `include_str!` and assert no secret-returning
command appears in it. A test that reads its own source is unusual, but the
thing being protected is the *list*, not the behaviour of any one function.

Be honest about the residual gap in the same breath: this stops a webview
compromise from exfiltrating passwords. It does not stop one from connecting as
the user, and it does not contain a compromise of the native side.

### 18.15 Validate the request before looking up the connection

A small ordering rule with an outsized effect on debuggability. This was a real
bug in `forwards::start`:

```
let transport = pool.get(key)?;          // "not connected: nope"
match kind { "remote" => { ...needs a target host... } }
```

A remote forward missing its target reported **"not connected"**, sending the
user off to debug SSH. A malformed request is malformed whether or not a
transport exists, so check the arguments first.

It surfaced only because a stale test was renamed to assert that the error
*names what is missing*. Assertions on error text are usually noise; for
user-facing errors they are the only thing that catches this class of bug.

### 18.16 Linux: the child inherits the launcher's environment

On Windows, ConPTY ignores `TERM`, so nothing ever set it. On Linux the shell
inherits whatever launched OpsPilot: **no `TERM` at all** from a desktop icon,
or the *parent* emulator's identity (`xterm-kitty`, `VTE_VERSION`) from a
terminal. Both are wrong, silently. Set `TERM`/`COLORTERM` explicitly and strip
other emulators' markers before spawn.

Related Linux facts that look like bugs elsewhere:

- `/bin` is a symlink to `/usr/bin` on every usr-merged distro, so the same
  binary has two paths. Dedupe by **canonical path**, and discover shells from
  `/etc/shells` rather than hardcoding locations.
- A closed PTY reads as `EIO`, not EOF; a signal mid-read is `EINTR` and is not
  the end of the session.
- `rename` is only durable once the **parent directory** is fsynced.
- WebKitGTK's DMA-BUF renderer breaks on the proprietary NVIDIA driver. Disable
  it only there — on Mesa it is the fast path.

### 18.17 ConPTY hid the whole input side

ConPTY consumes the application's output, tracks its modes, and re-encodes
input. So on Windows a terminal that ignores DECCKM (`?1`), bracketed paste
(`?2004`) and DECTCEM (`?25`) still appears to work. On a Unix PTY it does not:
ncurses enables application-cursor mode via terminfo `smkx` and then reads
`ESC[A` as garbage, so arrow keys break in htop, nano and mc. **zsh sets DECCKM at
every prompt.** Track these modes in `Screen::modes()` and encode input from them.

The same lesson applies to key bindings. ⌘ maps to Ctrl only for apps whose
window is not a terminal. Ctrl+W, Ctrl+K, Ctrl+B and Ctrl+I (which *is* Tab) all
belong to the shell, so off macOS app chords are Ctrl+Shift (D-24). Before binding
anything, check what the key sends to a shell.
