//! Progress and cancellation, shared between a transfer and whoever watches it.
//!
//! §13.1 rule 8: "Progress via `AtomicU64` sampled by the UI at 10 Hz. Never one
//! message per chunk — at 100 MB/s with 256 KB chunks that is 400 msg/s and it
//! will melt the frame budget." Nothing here pushes; everything is sampled.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::Notify;

/// How a transfer is being carried out. Shown in the UI: §6.2 asks for the
/// switch to be visible ("batched · 8,412 files"), because a user who waited
/// nine minutes in WinSCP deserves to know why this took eleven seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Method {
    #[default]
    Preparing,
    /// One file, many requests in flight (§6.1).
    Pipelined,
    /// One large upload over several channels, past the server's per-channel
    /// window (see `pipe::upload_striped`).
    Striped { channels: u32 },
    /// Many files over several SFTP channels on the same connection (§6.2).
    Parallel { channels: u32 },
    /// One `tar` stream over an exec channel (§6.2).
    TarPipe,
}

impl Method {
    pub fn label(&self) -> String {
        match self {
            Method::Preparing => "preparing".into(),
            Method::Pipelined => "pipelined".into(),
            Method::Striped { channels } => format!("striped · {channels} channels"),
            Method::Parallel { channels } => format!("parallel · {channels} channels"),
            Method::TarPipe => "batched · tar".into(),
        }
    }
}

#[derive(Default, Debug)]
pub struct Progress {
    pub bytes_total: AtomicU64,
    pub bytes_done: AtomicU64,
    pub files_total: AtomicU64,
    pub files_done: AtomicU64,
    /// Symlinks and special files the SFTP path does not copy.
    pub files_skipped: AtomicU64,
    /// The largest current request window across workers (§6.1), for the perf
    /// overlay's "outstanding SFTP requests".
    pub window: AtomicU32,
    method: Mutex<Method>,
    /// Why the method fell back, if it did ("tar not found on the server").
    note: Mutex<Option<String>>,
}

impl Progress {
    pub fn method(&self) -> Method {
        self.method.lock().map(|m| m.clone()).unwrap_or_default()
    }

    pub fn set_method(&self, m: Method) {
        if let Ok(mut g) = self.method.lock() {
            *g = m;
        }
    }

    pub fn note(&self) -> Option<String> {
        self.note.lock().ok().and_then(|n| n.clone())
    }

    pub fn set_note(&self, n: impl Into<String>) {
        if let Ok(mut g) = self.note.lock() {
            *g = Some(n.into());
        }
    }

    pub(crate) fn add_bytes(&self, n: u64) {
        self.bytes_done.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn file_done(&self) {
        self.files_done.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn show_window(&self, w: u32) {
        self.window.fetch_max(w, Ordering::Relaxed);
    }
}

/// Cooperative cancellation. Every transfer loop selects on [`Cancel::wait`],
/// so closing the transfer stops it mid-request rather than at the next file.
#[derive(Default, Debug)]
pub struct Cancel {
    flag: AtomicBool,
    notify: Notify,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Resolves once cancelled. Safe to race against: the flag is checked
    /// after registering for the notification, so a cancel in between is not
    /// missed.
    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}
