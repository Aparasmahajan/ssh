//! Docker commands: one live [`Watch`] per process, created on first use.
//!
//! §14.5: nothing here runs until the Docker module is opened — no socket is
//! touched at startup. If the daemon is down (Docker Desktop not started), the
//! endpoint is re-tried at most every few seconds rather than on every UI
//! sample.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use opspilot_core_docker::{self as docker, endpoint, Action, Api, Watch};
use serde::Serialize;

use crate::remote::runtime;
use crate::session::{registry, SessionId, StreamIo, STREAM_DEPTH};

struct Live {
    endpoint: endpoint::Endpoint,
    api: Api,
    watch: Watch,
}

#[derive(Default)]
struct Slot {
    live: Option<Live>,
    /// Last failure to even create a client, and when, for the retry gate.
    failed: Option<(String, Instant)>,
    /// The endpoint last tried, reported while the daemon is unreachable
    /// without re-reading the docker config on every UI sample.
    endpoint: Option<endpoint::Endpoint>,
    /// When the UI last sampled. The watch — an events stream plus one stats
    /// stream per running container — is dropped after [`IDLE`] unsampled, so
    /// leaving the Docker panel does not leave fifty streams running.
    seen: Option<Instant>,
}

const IDLE: Duration = Duration::from_secs(60);

/// Drop the watch once nobody has looked for [`IDLE`]. Started once.
fn start_sweeper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        runtime().spawn(async {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tick.tick().await;
                if let Ok(mut s) = slot().lock() {
                    if s.seen.is_some_and(|t| t.elapsed() > IDLE) {
                        // Watch::drop aborts its task and every stats stream.
                        s.live = None;
                        s.seen = None;
                    }
                }
            }
        });
    });
}

/// Stop everything now (frontend reset).
pub fn stop() {
    if let Ok(mut s) = slot().lock() {
        s.live = None;
        s.seen = None;
    }
}

const RETRY: Duration = Duration::from_secs(3);

fn slot() -> &'static Mutex<Slot> {
    static S: OnceLock<Mutex<Slot>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// The client, connecting on first use. Holding the lock across `connect` is
/// fine: it only checks the socket exists and builds a client, no I/O.
fn ensure(s: &mut Slot) -> Result<&Live, String> {
    if s.live.is_none() {
        if let Some((e, at)) = &s.failed {
            if at.elapsed() < RETRY {
                return Err(e.clone());
            }
        }
        let resolved = endpoint::resolve();
        s.endpoint = resolved.as_ref().ok().cloned();
        let attempt = resolved.and_then(|ep| {
            let api = docker::connect(&ep)?;
            Ok((ep, api))
        });
        match attempt {
            Ok((endpoint, api)) => {
                let _guard = runtime().enter();
                let watch = Watch::start(api.clone(), runtime().handle());
                s.live = Some(Live {
                    endpoint,
                    api,
                    watch,
                });
                s.failed = None;
            }
            Err(e) => {
                s.failed = Some((e.clone(), Instant::now()));
                return Err(e);
            }
        }
    }
    s.live.as_ref().ok_or_else(|| "docker: not connected".to_string())
}

fn api() -> Result<Api, String> {
    let mut s = slot().lock().map_err(|_| "docker state poisoned")?;
    ensure(&mut s).map(|l| l.api.clone())
}

#[derive(Serialize)]
pub struct DockerView {
    /// "unix:///…/docker.sock" — shown so a wrong daemon is diagnosable.
    pub endpoint: Option<String>,
    pub source: Option<String>,
    #[serde(flatten)]
    pub snapshot: docker::Snapshot,
}

/// `None` when nothing changed since `since`.
pub fn snapshot(since: u64) -> Result<Option<DockerView>, String> {
    start_sweeper();
    let mut s = slot().lock().map_err(|_| "docker state poisoned")?;
    s.seen = Some(Instant::now());
    let tried = s.endpoint.clone();
    match ensure(&mut s) {
        Ok(live) => Ok(live.watch.snapshot(since).map(|snapshot| DockerView {
            endpoint: Some(live.endpoint.host.clone()),
            source: Some(live.endpoint.source.clone()),
            snapshot,
        })),
        // No client at all (socket missing). Reported as a snapshot with
        // generation 0, so the UI shows the reason in place of the list and
        // keeps asking with `since = 0` — the first live snapshot then arrives
        // on the next sample after Docker starts.
        Err(e) => Ok(Some(DockerView {
            endpoint: tried.as_ref().map(|ep| ep.host.clone()),
            source: tried.map(|ep| ep.source),
            snapshot: docker::Snapshot {
                generation: 0,
                connected: false,
                error: Some(e),
                version: None,
                containers: Vec::new(),
            },
        })),
    }
}

/// Run a future on the network runtime and wait for it from any async context.
///
/// Commands that wait on the daemon are `async` so they run off the main
/// thread: Tauri runs synchronous commands ON the main thread, and `docker
/// stop` waits out the container's 10 s grace period — a synchronous command
/// would freeze the window for all of it.
async fn on_runtime<T: Send + 'static>(
    f: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    runtime()
        .spawn(f)
        .await
        .map_err(|e| format!("docker task failed: {e}"))?
}

pub async fn action(id: String, action: String) -> Result<(), String> {
    let a = Action::parse(&action).ok_or_else(|| format!("unknown docker action `{action}`"))?;
    let api = api()?;
    on_runtime(async move { docker::act(&api, &id, a).await }).await
}

/// An interactive shell in a container, as a terminal session.
pub async fn exec(id: String, rows: u16, cols: u16) -> Result<SessionId, String> {
    let api = api()?;
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(STREAM_DEPTH);
    let (in_tx, in_rx) = tokio::sync::mpsc::channel(STREAM_DEPTH);
    let (rs_tx, rs_rx) = tokio::sync::mpsc::channel(4);
    on_runtime(async move { docker::exec(&api, &id, rows, cols, out_tx, in_rx, rs_rx).await }).await?;
    registry().open_stream(
        rows,
        cols,
        StreamIo {
            output: out_rx,
            input: Some(in_tx),
            resize: Some(rs_tx),
            // A TTY exec already emits CR LF.
            lf_to_crlf: false,
        },
        false,
    )
}

/// Follow a container's logs in a read-only terminal session.
pub fn logs(id: &str, rows: u16, cols: u16) -> Result<SessionId, String> {
    let api = api()?;
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(STREAM_DEPTH);
    {
        let _guard = runtime().enter();
        // 1,000 lines of history: enough context, bounded by the scrollback
        // ring anyway (§5.6).
        docker::logs(&api, id, 1000, out_tx)?;
    }
    registry().open_stream(
        rows,
        cols,
        StreamIo {
            output: out_rx,
            input: None,
            resize: None,
            lf_to_crlf: true,
        },
        false,
    )
}
