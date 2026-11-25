//! The transfer queue. CLAUDE.md §6, Step 6.
//!
//! Jobs run on the network runtime through `core-sftp`, at most
//! [`MAX_RUNNING`] at once; the rest wait in order. Every command here touches
//! memory only — start spawns, list reads atomics, cancel sets a flag — so all
//! of them are synchronous Tauri commands without breaking §18.19.
//!
//! §13.1 rule 8: the UI samples [`list`] at 10 Hz. A job never pushes a message
//! per chunk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use opspilot_core_sftp::{self as engine, Cancel, Error, Options, Progress, Request};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::remote::{self, runtime};

/// Transfers running at once. Each can use up to 8 channels; more than two
/// big ones at a time only split the same link.
const MAX_RUNNING: usize = 2;
/// Finished jobs kept for the list.
const KEEP_FINISHED: usize = 200;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Clone, Debug)]
enum State {
    Queued,
    Running,
    Done(engine::Summary),
    Failed { error: String, exists: bool },
    Cancelled,
}

struct Job {
    id: u64,
    key: String,
    direction: Direction,
    name: String,
    source: String,
    target_dir: String,
    overwrite: bool,
    progress: Arc<Progress>,
    cancel: Arc<Cancel>,
    state: Mutex<State>,
    started: Mutex<Option<Instant>>,
    elapsed_ms: AtomicU64,
}

#[derive(Default)]
struct Queue {
    jobs: Mutex<Vec<Arc<Job>>>,
    next: AtomicU64,
}

fn queue() -> &'static Queue {
    static Q: OnceLock<Queue> = OnceLock::new();
    Q.get_or_init(Queue::default)
}

fn slots() -> &'static Arc<Semaphore> {
    static S: OnceLock<Arc<Semaphore>> = OnceLock::new();
    S.get_or_init(|| Arc::new(Semaphore::new(MAX_RUNNING)))
}

fn base_name(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Queue a transfer. `source` is a remote path for a download and a local one
/// for an upload; `target_dir` is the directory it lands in, on the other side.
pub fn start(key: &str, direction: Direction, source: &str, target_dir: &str, overwrite: bool) -> Result<u64, String> {
    // Resolve the connection now, so a stale key is an error at the click
    // rather than a failed job later.
    let transport = remote::get(key)?;
    if source.is_empty() || target_dir.is_empty() {
        return Err("a transfer needs a source and a target directory".into());
    }
    let q = queue();
    let id = q.next.fetch_add(1, Ordering::Relaxed) + 1;
    let job = Arc::new(Job {
        id,
        key: key.to_string(),
        direction,
        name: base_name(source),
        source: source.to_string(),
        target_dir: target_dir.to_string(),
        overwrite,
        progress: Arc::new(Progress::default()),
        cancel: Arc::new(Cancel::default()),
        state: Mutex::new(State::Queued),
        started: Mutex::new(None),
        elapsed_ms: AtomicU64::new(0),
    });
    {
        let mut jobs = q.jobs.lock().map_err(|_| "transfer queue poisoned")?;
        jobs.push(Arc::clone(&job));
        prune(&mut jobs);
    }

    let request = match direction {
        Direction::Download => Request::Download { remote: job.source.clone(), local_dir: PathBuf::from(&job.target_dir) },
        Direction::Upload => Request::Upload { local: PathBuf::from(&job.source), remote_dir: job.target_dir.clone() },
    };
    runtime().spawn(async move {
        let permit = tokio::select! {
            p = Arc::clone(slots()).acquire_owned() => p,
            _ = job.cancel.wait() => {
                set(&job, State::Cancelled);
                return;
            }
        };
        let Ok(_permit) = permit else { return };
        set(&job, State::Running);
        let at = Instant::now();
        if let Ok(mut s) = job.started.lock() {
            *s = Some(at);
        }
        let options = Options { overwrite: job.overwrite, ..Default::default() };
        let r = engine::run(transport, request, options, Arc::clone(&job.progress), Arc::clone(&job.cancel)).await;
        job.elapsed_ms.store(at.elapsed().as_millis() as u64, Ordering::Relaxed);
        set(
            &job,
            match r {
                Ok(summary) => State::Done(summary),
                Err(Error::Cancelled) => State::Cancelled,
                Err(e) => State::Failed { exists: matches!(e, Error::Exists(_)), error: e.to_string() },
            },
        );
    });
    Ok(id)
}

fn set(job: &Job, s: State) {
    if let Ok(mut g) = job.state.lock() {
        *g = s;
    }
}

/// Drop the oldest finished jobs past [`KEEP_FINISHED`]. O(jobs).
fn prune(jobs: &mut Vec<Arc<Job>>) {
    let finished = |j: &Arc<Job>| {
        j.state
            .lock()
            .map(|s| matches!(*s, State::Done(_) | State::Failed { .. } | State::Cancelled))
            .unwrap_or(true)
    };
    let mut excess = jobs.iter().filter(|j| finished(j)).count().saturating_sub(KEEP_FINISHED);
    jobs.retain(|j| {
        if excess > 0 && finished(j) {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

pub fn cancel(id: u64) -> Result<(), String> {
    let jobs = queue().jobs.lock().map_err(|_| "transfer queue poisoned")?;
    let job = jobs.iter().find(|j| j.id == id).ok_or_else(|| format!("no transfer {id}"))?;
    job.cancel.cancel();
    Ok(())
}

/// Forget finished jobs.
pub fn clear() {
    if let Ok(mut jobs) = queue().jobs.lock() {
        jobs.retain(|j| j.state.lock().map(|s| matches!(*s, State::Queued | State::Running)).unwrap_or(false));
    }
}

/// Cancel everything (app shutdown / reset).
pub fn cancel_all() -> usize {
    let Ok(jobs) = queue().jobs.lock() else { return 0 };
    let mut n = 0;
    for j in jobs.iter() {
        if j.state.lock().map(|s| matches!(*s, State::Queued | State::Running)).unwrap_or(false) {
            j.cancel.cancel();
            n += 1;
        }
    }
    n
}

/// One job, as the UI sees it. Sampled, never pushed.
#[derive(Serialize, Clone, Debug)]
pub struct TransferView {
    pub id: u64,
    pub key: String,
    pub direction: Direction,
    pub name: String,
    pub source: String,
    pub target_dir: String,
    /// "queued" | "running" | "done" | "failed" | "cancelled"
    pub state: &'static str,
    pub error: Option<String>,
    /// Failed only because the target exists: the UI offers to replace it.
    pub exists: bool,
    /// "pipelined", "striped · 8 channels", "batched · tar", …
    pub method: String,
    /// Why the method changed, if it did.
    pub note: Option<String>,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub files_total: u64,
    pub files_done: u64,
    pub skipped: u64,
    /// Peak request window (§6.1), for the perf overlay.
    pub window: u32,
    pub elapsed_ms: u64,
}

/// Every job, oldest first. O(jobs), no I/O.
pub fn list() -> Vec<TransferView> {
    let Ok(jobs) = queue().jobs.lock() else { return Vec::new() };
    jobs.iter()
        .map(|j| {
            let state = j.state.lock().map(|s| s.clone()).unwrap_or(State::Cancelled);
            let p = &j.progress;
            let elapsed_ms = match (&state, j.started.lock().ok().and_then(|s| *s)) {
                (State::Running, Some(at)) => at.elapsed().as_millis() as u64,
                _ => j.elapsed_ms.load(Ordering::Relaxed),
            };
            let (label, error, exists, method) = match &state {
                State::Queued => ("queued", None, false, "queued".to_string()),
                State::Running => ("running", None, false, p.method().label()),
                State::Done(s) => ("done", None, false, s.method.label()),
                State::Failed { error, exists } => ("failed", Some(error.clone()), *exists, p.method().label()),
                State::Cancelled => ("cancelled", None, false, p.method().label()),
            };
            TransferView {
                id: j.id,
                key: j.key.clone(),
                direction: j.direction,
                name: j.name.clone(),
                source: j.source.clone(),
                target_dir: j.target_dir.clone(),
                state: label,
                error,
                exists,
                method,
                note: p.note(),
                bytes_total: p.bytes_total.load(Ordering::Relaxed),
                bytes_done: p.bytes_done.load(Ordering::Relaxed),
                files_total: p.files_total.load(Ordering::Relaxed),
                files_done: p.files_done.load(Ordering::Relaxed),
                skipped: p.files_skipped.load(Ordering::Relaxed),
                window: p.window.load(Ordering::Relaxed),
                elapsed_ms,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_the_last_component_on_either_side() {
        assert_eq!(base_name("/srv/app/"), "app");
        assert_eq!(base_name("/srv/app/x.log"), "x.log");
        assert_eq!(base_name(r"C:\Users\me\file.txt"), "file.txt");
    }

    #[test]
    fn an_unknown_connection_is_refused_at_the_click() {
        let e = start("nobody@nowhere:22#x", Direction::Download, "/a", "/b", false).unwrap_err();
        assert!(e.contains("not connected"), "{e}");
    }
}
