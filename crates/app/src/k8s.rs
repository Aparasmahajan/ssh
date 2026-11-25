//! Kubernetes commands.
//!
//! One client per context, one informer per (context, kind, namespace). An
//! informer nobody has sampled for [`IDLE`] is dropped, which closes its watch:
//! browsing through kinds and namespaces must not leave a trail of open
//! watches on the API server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use opspilot_core_kube::{self as k, Contexts, Informer, Kind, Snapshot};

use crate::remote::runtime;
use crate::session::{registry, SessionId, StreamIo, STREAM_DEPTH};

const IDLE: Duration = Duration::from_secs(60);

type Key = (String, Kind, String);

#[derive(Default)]
struct State {
    clients: HashMap<String, kube::Client>,
    informers: HashMap<Key, (Arc<Informer>, Instant)>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// API calls that are not watches get this long. kube's default read timeout
/// is none, so an API server (or a load balancer in front of one) that accepts
/// and never answers would leave the UI spinner — and the task — forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

async fn on_runtime<T: Send + 'static>(
    f: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    runtime()
        .spawn(async move {
            tokio::time::timeout(CALL_TIMEOUT, f)
                .await
                .map_err(|_| format!("no answer from the API server after {} s", CALL_TIMEOUT.as_secs()))?
        })
        .await
        .map_err(|e| format!("kubernetes task failed: {e}"))?
}

/// Sweep idle informers on a timer. Sweeping only inside `snapshot` meant the
/// last informers viewed — watches open on the API server, every row in
/// memory — lived until the panel was opened again. Started once.
fn start_sweeper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        runtime().spawn(async {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tick.tick().await;
                if let Ok(mut st) = state().lock() {
                    st.informers.retain(|_, (_, seen)| seen.elapsed() < IDLE);
                }
            }
        });
    });
}

/// Drop every informer and cached client now (frontend reset).
pub fn stop() {
    if let Ok(mut st) = state().lock() {
        st.informers.clear();
        st.clients.clear();
    }
}

pub fn contexts() -> Result<Contexts, String> {
    k::contexts()
}

/// The client for a context, created on first use. Creating one can run an
/// exec auth plugin (`aws eks get-token`, `gke-gcloud-auth-plugin`), so it is
/// async and never on the main thread.
async fn client(context: &str) -> Result<kube::Client, String> {
    if let Some(c) = state().lock().map_err(|_| "kube state poisoned")?.clients.get(context) {
        return Ok(c.clone());
    }
    let ctx = context.to_string();
    let c = on_runtime(async move { k::client(&ctx).await }).await?;
    state()
        .lock()
        .map_err(|_| "kube state poisoned")?
        .clients
        .insert(context.to_string(), c.clone());
    Ok(c)
}

fn kind(s: &str) -> Result<Kind, String> {
    Kind::parse(s).ok_or_else(|| format!("unknown kind `{s}`"))
}

pub async fn snapshot(context: String, kind_s: String, namespace: String, since: u64) -> Result<Option<Snapshot>, String> {
    start_sweeper();
    let kind = kind(&kind_s)?;
    let ns = if kind.namespaced() { namespace } else { String::new() };
    let key = (context.clone(), kind, ns.clone());

    let existing = {
        let mut st = state().lock().map_err(|_| "kube state poisoned")?;
        // Sweep idle informers. O(informers), a handful.
        st.informers.retain(|_, (_, seen)| seen.elapsed() < IDLE);
        st.informers.get_mut(&key).map(|(inf, seen)| {
            *seen = Instant::now();
            Arc::clone(inf)
        })
    };
    let inf = match existing {
        Some(i) => i,
        None => {
            let c = client(&context).await?;
            let inf = Arc::new(Informer::start(c, kind, &ns, runtime().handle()));
            state()
                .lock()
                .map_err(|_| "kube state poisoned")?
                .informers
                .insert(key, (Arc::clone(&inf), Instant::now()));
            // A new informer has nothing yet; report from generation 0.
            return Ok(inf.snapshot(0));
        }
    };
    Ok(inf.snapshot(since))
}

pub async fn action(
    context: String,
    action: String,
    namespace: String,
    name: String,
    replicas: Option<i32>,
) -> Result<(), String> {
    let c = client(&context).await?;
    on_runtime(async move {
        match action.as_str() {
            "delete-pod" => k::delete_pod(c, &namespace, &name).await,
            "scale" => k::scale(c, &namespace, &name, replicas.ok_or("scale needs replicas")?).await,
            "rollout-restart" => k::rollout_restart(c, &namespace, &name).await,
            other => Err(format!("unknown kubernetes action `{other}`")),
        }
    })
    .await
}

pub async fn yaml(context: String, kind_s: String, namespace: String, name: String) -> Result<String, String> {
    let kind = kind(&kind_s)?;
    let c = client(&context).await?;
    on_runtime(async move { k::yaml(c, kind, &namespace, &name).await }).await
}

pub async fn exec(
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
    rows: u16,
    cols: u16,
) -> Result<SessionId, String> {
    let c = client(&context).await?;
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(STREAM_DEPTH);
    let (in_tx, in_rx) = tokio::sync::mpsc::channel(STREAM_DEPTH);
    let (rs_tx, rs_rx) = tokio::sync::mpsc::channel(4);
    on_runtime(async move { k::exec(c, &namespace, &pod, container, rows, cols, out_tx, in_rx, rs_rx).await })
        .await?;
    registry().open_stream(
        rows,
        cols,
        StreamIo {
            output: out_rx,
            input: Some(in_tx),
            resize: Some(rs_tx),
            lf_to_crlf: false,
        },
        true,
    )
}

pub async fn logs(
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
    rows: u16,
    cols: u16,
) -> Result<SessionId, String> {
    let c = client(&context).await?;
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(STREAM_DEPTH);
    on_runtime(async move { k::logs(c, &namespace, &pod, container, 1000, out_tx).await }).await?;
    registry().open_stream(
        rows,
        cols,
        StreamIo {
            output: out_rx,
            input: None,
            resize: None,
            lf_to_crlf: true,
        },
        true,
    )
}
