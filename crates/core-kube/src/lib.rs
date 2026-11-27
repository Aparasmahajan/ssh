//! Kubernetes for OpsPilot.
//!
//! CLAUDE.md §10, as built:
//!
//! - **Watch, don't poll.** One informer per (context, kind, namespace): a
//!   `kube_runtime::watcher` feeding a UID-keyed store. The UI samples it by
//!   generation (§13.1 rule 8). Lens polls; that is why Lens is slow.
//! - **WatchList bootstrap**, feature-detected: the first attempt uses
//!   `sendInitialEvents` (streaming lists), and an informer whose server rejects
//!   it falls back to List + Watch for the rest of its life.
//! - **Dedup by UID** — a pod that changes fifty times between samples is one
//!   store entry and one repaint, which is the point of §10's DeltaFIFO.
//!
//! **Not** protobuf: §10 asks for `application/vnd.kubernetes.protobuf`, but
//! kube-rs decodes JSON only. The watch-not-poll and UID-dedup decisions are the
//! ones that dominate; see docs/DECISIONS.md D-29.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Namespace, Node, Pod, Service};
use kube::api::{Api, AttachParams, DeleteParams, LogParams, Patch, PatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::runtime::watcher::{self, Event};
use kube::runtime::WatchStreamExt;
use kube::{Client, Config, Resource, ResourceExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/* ---- kubeconfig ------------------------------------------------------------- */

#[derive(Serialize, Clone, Debug)]
pub struct ContextInfo {
    pub name: String,
    pub cluster: String,
    pub user: String,
    pub namespace: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Contexts {
    pub current: Option<String>,
    pub contexts: Vec<ContextInfo>,
    /// Where it was read from, for the UI: `$KUBECONFIG` or `~/.kube/config`.
    pub source: String,
}

/// Read kubeconfig the way kubectl does: `$KUBECONFIG` (merged list) or
/// `~/.kube/config`.
pub fn contexts() -> Result<Contexts, String> {
    let source = std::env::var("KUBECONFIG").unwrap_or_else(|_| "~/.kube/config".into());
    let kc = Kubeconfig::read().map_err(|e| format!("kubeconfig ({source}): {e}"))?;
    Ok(Contexts {
        current: kc.current_context.clone(),
        contexts: kc
            .contexts
            .iter()
            .map(|c| ContextInfo {
                name: c.name.clone(),
                cluster: c.context.as_ref().map(|x| x.cluster.clone()).unwrap_or_default(),
                user: c.context.as_ref().and_then(|x| x.user.clone()).unwrap_or_default(),
                namespace: c.context.as_ref().and_then(|x| x.namespace.clone()),
            })
            .collect(),
        source,
    })
}

pub async fn client(context: &str) -> Result<Client, String> {
    let kc = Kubeconfig::read().map_err(|e| e.to_string())?;
    let cfg = Config::from_custom_kubeconfig(
        kc,
        &KubeConfigOptions {
            context: Some(context.to_string()),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| format!("context {context}: {e}"))?;
    Client::try_from(cfg).map_err(err)
}

/* ---- Errors ------------------------------------------------------------------- */

/// Every credential failure's message starts with this, so the app can tell
/// "log in again" from "the request was wrong" without a second error type
/// crossing the crate boundary. Check with [`is_unauthorized`].
const UNAUTHORIZED: &str = "unauthorized (401)";

/// True when the server rejected the credentials, or they could not be
/// produced (an exec auth plugin failed). Either way the cached client is
/// stale: a new one re-reads kubeconfig and re-runs the plugin.
fn rejected_credentials(e: &kube::Error) -> bool {
    match e {
        kube::Error::Api(s) => s.code == 401,
        kube::Error::Auth(_) => true,
        kube::Error::UpgradeConnection(kube::client::UpgradeConnectionError::ProtocolSwitch(c)) => {
            c.as_u16() == 401
        }
        _ => false,
    }
}

/// The one place a `kube::Error` becomes a message.
fn err(e: kube::Error) -> String {
    if rejected_credentials(&e) {
        format!("{UNAUTHORIZED}: the cluster rejected this context's credentials ({e})")
    } else {
        e.to_string()
    }
}

fn watch_rejected_credentials(e: &watcher::Error) -> bool {
    match e {
        watcher::Error::InitialListFailed(e)
        | watcher::Error::WatchStartFailed(e)
        | watcher::Error::WatchFailed(e) => rejected_credentials(e),
        watcher::Error::WatchError(s) => s.code == 401,
        watcher::Error::NoResourceVersion => false,
    }
}

/// Whether an error message from this crate means the credentials were
/// rejected — the signal to drop the cached client.
pub fn is_unauthorized(msg: &str) -> bool {
    msg.starts_with(UNAUTHORIZED)
}

/* ---- Rows --------------------------------------------------------------------- */

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pods,
    Deployments,
    Services,
    Nodes,
    Namespaces,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "pods" => Kind::Pods,
            "deployments" => Kind::Deployments,
            "services" => Kind::Services,
            "nodes" => Kind::Nodes,
            "namespaces" => Kind::Namespaces,
            _ => return None,
        })
    }

    pub fn namespaced(self) -> bool {
        !matches!(self, Kind::Nodes | Kind::Namespaces)
    }

    /// Column headers after name and namespace; `Row::cells` matches.
    pub fn columns(self) -> &'static [&'static str] {
        match self {
            Kind::Pods => &["ready", "status", "restarts", "node", "ip"],
            Kind::Deployments => &["ready", "up-to-date", "available", "images"],
            Kind::Services => &["type", "cluster-ip", "external-ip", "ports"],
            Kind::Nodes => &["status", "roles", "version", "internal-ip"],
            Kind::Namespaces => &["status"],
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Row {
    pub uid: String,
    pub name: String,
    pub namespace: String,
    pub cells: Vec<String>,
    /// Unix seconds; the UI renders age so it ticks without events.
    pub created: i64,
    /// live | warn | bad | idle
    pub tone: &'static str,
    /// Pods: container names, for logs and exec.
    pub containers: Vec<String>,
    /// Deployments: desired replicas, for the scale control.
    pub replicas: Option<i32>,
}

fn created<K: Resource>(o: &K) -> i64 {
    o.meta().creation_timestamp.as_ref().map(|t| t.0.as_second()).unwrap_or(0)
}

fn base<K: Resource>(o: &K) -> (String, String, String) {
    let m = o.meta();
    (
        m.uid.clone().unwrap_or_default(),
        m.name.clone().unwrap_or_default(),
        m.namespace.clone().unwrap_or_default(),
    )
}

/// kubectl's STATUS column, which is not `status.phase`: a pod in
/// CrashLoopBackOff has phase Running, and that is exactly the pod you are
/// looking for.
pub fn pod_status(p: &Pod) -> (String, &'static str) {
    if p.metadata.deletion_timestamp.is_some() {
        return ("Terminating".into(), "warn");
    }
    let st = p.status.as_ref();
    let phase = st.and_then(|s| s.phase.clone()).unwrap_or_else(|| "Unknown".into());
    if let Some(reason) = st.and_then(|s| s.reason.clone()) {
        return (reason, "bad"); // Evicted, NodeLost, …
    }
    let statuses = st.and_then(|s| s.container_statuses.as_ref());
    for cs in statuses.into_iter().flatten() {
        let state = cs.state.as_ref();
        if let Some(w) = state.and_then(|s| s.waiting.as_ref()) {
            if let Some(r) = &w.reason {
                let tone = if r == "ContainerCreating" || r == "PodInitializing" { "warn" } else { "bad" };
                return (r.clone(), tone);
            }
        }
        if let Some(t) = state.and_then(|s| s.terminated.as_ref()) {
            if let Some(r) = &t.reason {
                if r != "Completed" {
                    return (r.clone(), "bad");
                }
            }
        }
    }
    let ready = statuses.is_some_and(|v| !v.is_empty() && v.iter().all(|c| c.ready));
    let tone = match phase.as_str() {
        "Running" if ready => "live",
        "Running" | "Pending" => "warn",
        "Succeeded" => "idle",
        "Failed" | "Unknown" => "bad",
        _ => "idle",
    };
    let label = if phase == "Succeeded" { "Completed".to_string() } else { phase };
    (label, tone)
}

pub fn pod_row(p: &Pod) -> Row {
    let (uid, name, namespace) = base(p);
    let st = p.status.as_ref();
    let statuses = st.and_then(|s| s.container_statuses.clone()).unwrap_or_default();
    let total = p.spec.as_ref().map(|s| s.containers.len()).unwrap_or(statuses.len());
    let ready = statuses.iter().filter(|c| c.ready).count();
    let restarts: i32 = statuses.iter().map(|c| c.restart_count).sum();
    let (status, tone) = pod_status(p);
    Row {
        uid,
        name,
        namespace,
        cells: vec![
            format!("{ready}/{total}"),
            status,
            restarts.to_string(),
            p.spec.as_ref().and_then(|s| s.node_name.clone()).unwrap_or_default(),
            st.and_then(|s| s.pod_ip.clone()).unwrap_or_default(),
        ],
        created: created(p),
        tone,
        containers: p
            .spec
            .as_ref()
            .map(|s| s.containers.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default(),
        replicas: None,
    }
}

pub fn deployment_row(d: &Deployment) -> Row {
    let (uid, name, namespace) = base(d);
    let want = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
    let st = d.status.as_ref();
    let ready = st.and_then(|s| s.ready_replicas).unwrap_or(0);
    let updated = st.and_then(|s| s.updated_replicas).unwrap_or(0);
    let available = st.and_then(|s| s.available_replicas).unwrap_or(0);
    let images = d
        .spec
        .as_ref()
        .and_then(|s| s.template.spec.as_ref())
        .map(|s| s.containers.iter().filter_map(|c| c.image.clone()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    let tone = if want == 0 {
        "idle"
    } else if available >= want && ready >= want {
        "live"
    } else if available == 0 {
        "bad"
    } else {
        "warn"
    };
    Row {
        uid,
        name,
        namespace,
        cells: vec![format!("{ready}/{want}"), updated.to_string(), available.to_string(), images],
        created: created(d),
        tone,
        containers: Vec::new(),
        replicas: Some(want),
    }
}

pub fn service_row(s: &Service) -> Row {
    let (uid, name, namespace) = base(s);
    let spec = s.spec.as_ref();
    let external = s
        .status
        .as_ref()
        .and_then(|st| st.load_balancer.as_ref())
        .and_then(|lb| lb.ingress.as_ref())
        .map(|i| {
            i.iter()
                .filter_map(|x| x.ip.clone().or_else(|| x.hostname.clone()))
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|s| !s.is_empty())
        .or_else(|| spec.and_then(|sp| sp.external_ips.as_ref()).map(|v| v.join(",")))
        .unwrap_or_else(|| "<none>".into());
    let ports = spec
        .and_then(|sp| sp.ports.as_ref())
        .map(|ps| {
            ps.iter()
                .map(|p| {
                    let proto = p.protocol.clone().unwrap_or_else(|| "TCP".into());
                    match p.node_port {
                        Some(np) => format!("{}:{np}/{proto}", p.port),
                        None => format!("{}/{proto}", p.port),
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    Row {
        uid,
        name,
        namespace,
        cells: vec![
            spec.and_then(|sp| sp.type_.clone()).unwrap_or_else(|| "ClusterIP".into()),
            spec.and_then(|sp| sp.cluster_ip.clone()).unwrap_or_default(),
            external,
            ports,
        ],
        created: created(s),
        tone: "live",
        containers: Vec::new(),
        replicas: None,
    }
}

pub fn node_row(n: &Node) -> Row {
    let (uid, name, _) = base(n);
    let st = n.status.as_ref();
    let ready = st
        .and_then(|s| s.conditions.as_ref())
        .and_then(|cs| cs.iter().find(|c| c.type_ == "Ready"))
        .map(|c| c.status == "True")
        .unwrap_or(false);
    let unschedulable = n.spec.as_ref().and_then(|s| s.unschedulable).unwrap_or(false);
    let mut status = if ready { "Ready".to_string() } else { "NotReady".to_string() };
    if unschedulable {
        status.push_str(",SchedulingDisabled");
    }
    let roles = n
        .labels()
        .keys()
        .filter_map(|k| k.strip_prefix("node-role.kubernetes.io/"))
        .collect::<Vec<_>>()
        .join(",");
    let ip = st
        .and_then(|s| s.addresses.as_ref())
        .and_then(|a| a.iter().find(|x| x.type_ == "InternalIP"))
        .map(|a| a.address.clone())
        .unwrap_or_default();
    Row {
        uid,
        name,
        namespace: String::new(),
        cells: vec![
            status,
            if roles.is_empty() { "<none>".into() } else { roles },
            st.and_then(|s| s.node_info.as_ref()).map(|i| i.kubelet_version.clone()).unwrap_or_default(),
            ip,
        ],
        created: created(n),
        tone: if !ready { "bad" } else if unschedulable { "warn" } else { "live" },
        containers: Vec::new(),
        replicas: None,
    }
}

pub fn namespace_row(n: &Namespace) -> Row {
    let (uid, name, _) = base(n);
    let phase = n.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_else(|| "Active".into());
    Row {
        uid,
        name,
        namespace: String::new(),
        tone: if phase == "Active" { "live" } else { "warn" },
        cells: vec![phase],
        created: created(n),
        containers: Vec::new(),
        replicas: None,
    }
}

/* ---- Informer ------------------------------------------------------------------ */

#[derive(Default)]
struct State {
    /// UID -> row. Overwriting by key is the dedup.
    rows: HashMap<String, Row>,
    /// Rows being collected during a (re)list, swapped in at `InitDone`, so a
    /// relist after `410 Gone` never shows a half-empty table.
    pending: Option<HashMap<String, Row>>,
    synced: bool,
    error: Option<String>,
    /// The last error was a credential rejection. kube-runtime's backoff would
    /// retry with the same stale client forever; the app rebuilds it instead.
    unauthorized: bool,
    /// "watchlist" or "list+watch" — shown in the UI and the perf overlay.
    mode: &'static str,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub kind: Kind,
    pub columns: Vec<&'static str>,
    pub synced: bool,
    pub error: Option<String>,
    pub mode: &'static str,
    pub rows: Vec<Row>,
}

pub struct Informer {
    kind: Kind,
    started: std::time::Instant,
    generation: Arc<AtomicU64>,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for Informer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Informer {
    /// Start watching. `namespace` empty = all namespaces (or cluster-scoped).
    pub fn start(client: Client, kind: Kind, namespace: &str, rt: &tokio::runtime::Handle) -> Informer {
        let generation = Arc::new(AtomicU64::new(1));
        let state = Arc::new(Mutex::new(State {
            mode: "watchlist",
            ..Default::default()
        }));
        let ns = namespace.to_string();
        let (g, s) = (Arc::clone(&generation), Arc::clone(&state));
        let task = match kind {
            Kind::Pods => rt.spawn(run(ns_api::<Pod>(client, &ns), pod_row, s, g)),
            Kind::Deployments => rt.spawn(run(ns_api::<Deployment>(client, &ns), deployment_row, s, g)),
            Kind::Services => rt.spawn(run(ns_api::<Service>(client, &ns), service_row, s, g)),
            // Cluster-scoped: a namespace means nothing to them.
            Kind::Nodes => rt.spawn(run(Api::<Node>::all(client), node_row, s, g)),
            Kind::Namespaces => rt.spawn(run(Api::<Namespace>::all(client), namespace_row, s, g)),
        };
        Informer {
            kind,
            started: std::time::Instant::now(),
            generation,
            state,
            task,
        }
    }

    /// The watch is failing because the credentials were rejected.
    pub fn unauthorized(&self) -> bool {
        self.state.lock().map(|g| g.unauthorized).unwrap_or(false)
    }

    /// Time since this informer started, so a rebuild after a credential
    /// failure can be rate-limited.
    pub fn age(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    pub fn snapshot(&self, since: u64) -> Option<Snapshot> {
        let generation = self.generation.load(Ordering::Acquire);
        if generation == since {
            return None;
        }
        let g = self.state.lock().ok()?;
        let mut rows: Vec<Row> = g.rows.values().cloned().collect();
        rows.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
        Some(Snapshot {
            generation,
            kind: self.kind,
            columns: self.kind.columns().to_vec(),
            synced: g.synced,
            error: g.error.clone(),
            mode: g.mode,
            rows,
        })
    }
}

/// A namespaced resource: all namespaces, or one.
fn ns_api<K>(client: Client, ns: &str) -> Api<K>
where
    K: Resource<DynamicType = (), Scope = kube::core::NamespaceResourceScope>,
{
    if ns.is_empty() {
        Api::all(client)
    } else {
        Api::namespaced(client, ns)
    }
}

async fn run<K>(
    api: Api<K>,
    to_row: fn(&K) -> Row,
    state: Arc<Mutex<State>>,
    generation: Arc<AtomicU64>,
) where
    K: Resource + Clone + DeserializeOwned + std::fmt::Debug + Send + Sync + 'static,
{
    let mut streaming = true;
    loop {
        let cfg = if streaming {
            watcher::Config::default().streaming_lists()
        } else {
            watcher::Config::default()
        };
        // default_backoff: §8.3's exponential backoff, applied by kube-runtime
        // to every reconnect and relist.
        let mut stream = watcher::watcher(api.clone(), cfg).default_backoff().boxed();
        let mut ever_synced = false;
        while let Some(ev) = stream.next().await {
            match ev {
                Ok(ev) => {
                    apply(&state, ev, to_row, &mut ever_synced);
                }
                Err(e) => {
                    let unauthorized = watch_rejected_credentials(&e);
                    let msg = if unauthorized {
                        format!("{UNAUTHORIZED}: the cluster rejected this context's credentials ({e})")
                    } else {
                        e.to_string()
                    };
                    if let Ok(mut g) = state.lock() {
                        g.error = Some(msg);
                        g.unauthorized = unauthorized;
                    }
                    generation.fetch_add(1, Ordering::Release);
                    // WatchList unsupported (old server, or feature gate off):
                    // fall back once and for all. Other errors retry in place.
                    if streaming && !ever_synced {
                        streaming = false;
                        if let Ok(mut g) = state.lock() {
                            g.mode = "list+watch";
                        }
                        break;
                    }
                    continue;
                }
            }
            generation.fetch_add(1, Ordering::Release);
        }
    }
}

fn apply<K: Resource>(state: &Mutex<State>, ev: Event<K>, to_row: fn(&K) -> Row, ever_synced: &mut bool) {
    let Ok(mut g) = state.lock() else { return };
    match ev {
        Event::Init => g.pending = Some(HashMap::new()),
        Event::InitApply(o) => {
            let r = to_row(&o);
            match g.pending.as_mut() {
                Some(p) => {
                    p.insert(r.uid.clone(), r);
                }
                None => {
                    g.rows.insert(r.uid.clone(), r);
                }
            }
        }
        Event::InitDone => {
            if let Some(p) = g.pending.take() {
                g.rows = p;
            }
            g.synced = true;
            g.error = None;
            g.unauthorized = false;
            *ever_synced = true;
        }
        Event::Apply(o) => {
            let r = to_row(&o);
            g.rows.insert(r.uid.clone(), r);
            g.error = None;
            g.unauthorized = false;
        }
        Event::Delete(o) => {
            let uid = o.meta().uid.clone().unwrap_or_default();
            g.rows.remove(&uid);
        }
    }
}

/* ---- Actions ---------------------------------------------------------------------- */

/// A Kubernetes object name: DNS-1123 subdomain. Checked before any request so
/// a bad name is a clear error, not a 404 for a path the user never meant.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.ends_with(|c: char| c.is_ascii_alphanumeric())
}

fn check(what: &str, s: &str) -> Result<(), String> {
    if valid_name(s) {
        Ok(())
    } else {
        Err(format!("invalid {what} `{}`", s.escape_debug()))
    }
}

pub async fn delete_pod(client: Client, ns: &str, name: &str) -> Result<(), String> {
    check("namespace", ns)?;
    check("pod", name)?;
    Api::<Pod>::namespaced(client, ns)
        .delete(name, &DeleteParams::default())
        .await
        .map(|_| ())
        .map_err(err)
}

pub async fn scale(client: Client, ns: &str, name: &str, replicas: i32) -> Result<(), String> {
    check("namespace", ns)?;
    check("deployment", name)?;
    if !(0..=10_000).contains(&replicas) {
        return Err(format!("replicas {replicas} out of range"));
    }
    let patch = serde_json::json!({ "spec": { "replicas": replicas } });
    Api::<Deployment>::namespaced(client, ns)
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map(|_| ())
        .map_err(err)
}

/// `kubectl rollout restart`: bump an annotation on the pod template, which
/// the controller treats as a new revision.
pub async fn rollout_restart(client: Client, ns: &str, name: &str) -> Result<(), String> {
    check("namespace", ns)?;
    check("deployment", name)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let patch = serde_json::json!({
        "spec": { "template": { "metadata": { "annotations": {
            "kubectl.kubernetes.io/restartedAt": rfc3339(now)
        }}}}
    });
    Api::<Deployment>::namespaced(client, ns)
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map(|_| ())
        .map_err(err)
}

fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// An object as YAML, minus `managedFields` — kilobytes of server bookkeeping
/// nobody reads, and the first thing `kubectl get -o yaml` users scroll past.
pub async fn yaml(client: Client, kind: Kind, ns: &str, name: &str) -> Result<String, String> {
    if kind.namespaced() {
        check("namespace", ns)?;
    }
    check("name", name)?;
    let mut v: serde_json::Value = match kind {
        Kind::Pods => to_value(Api::<Pod>::namespaced(client, ns).get(name).await),
        Kind::Deployments => to_value(Api::<Deployment>::namespaced(client, ns).get(name).await),
        Kind::Services => to_value(Api::<Service>::namespaced(client, ns).get(name).await),
        Kind::Nodes => to_value(Api::<Node>::all(client).get(name).await),
        Kind::Namespaces => to_value(Api::<Namespace>::all(client).get(name).await),
    }?;
    if let Some(m) = v.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        m.remove("managedFields");
    }
    serde_saphyr::to_string(&v).map_err(|e| e.to_string())
}

fn to_value<K: Serialize>(r: Result<K, kube::Error>) -> Result<serde_json::Value, String> {
    let k = r.map_err(err)?;
    serde_json::to_value(k).map_err(|e| e.to_string())
}

/* ---- Logs and exec: bytes for a terminal session ------------------------------------ */

pub async fn logs(
    client: Client,
    ns: &str,
    pod: &str,
    container: Option<String>,
    tail: i64,
    out: mpsc::Sender<Vec<u8>>,
) -> Result<(), String> {
    check("namespace", ns)?;
    check("pod", pod)?;
    let api = Api::<Pod>::namespaced(client, ns);
    let stream = api
        .log_stream(
            pod,
            &LogParams {
                container,
                follow: true,
                tail_lines: Some(tail),
                ..Default::default()
            },
        )
        .await
        .map_err(err)?;
    tokio::spawn(async move {
        let mut stream = Box::pin(stream);
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            tokio::select! {
                n = futures_util::AsyncReadExt::read(&mut stream, &mut buf) => match n {
                    Ok(0) => {
                        let _ = out.send(b"\r\n[log stream ended]\r\n".to_vec()).await;
                        break;
                    }
                    Ok(n) => {
                        if out.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = out.send(format!("\r\n[log stream error: {e}]\r\n").into_bytes()).await;
                        break;
                    }
                },
                _ = out.closed() => break,
            }
        }
    });
    Ok(())
}

/// Prefer bash, fall back to sh; set TERM first (exec has no env field).
/// `OPSPILOT_SESSION=<token>.<own pid>` is what [`EXEC_HANGUP`] finds the
/// session by; `exec` keeps the pid, so it is the shell's.
fn shell_command(token: &str) -> String {
    format!(
        "export TERM=xterm-256color COLORTERM=truecolor OPSPILOT_SESSION={token}.$$; \
         if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi"
    )
}

/// The same script core-docker runs, for the same reason: a closed exec does
/// not hang up what it started. Kubernetes `exec` has the same gap
/// (kubernetes/kubernetes#50080). One copy, in core-docker.
const EXEC_HANGUP: &str = include_str!("../../core-docker/src/exec_hangup.sh");

/// See `opspilot_core_docker::session_token`; duplicated rather than adding a
/// dependency between two engine crates for six lines.
fn session_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    format!("{:016x}", h.finish())
}

/// Hang up what the exec session `token` left running. Best effort, bounded:
/// a pod that is gone or has no `sh` has nothing this could do anyway.
async fn hang_up(api: Api<Pod>, pod: String, container: Option<String>, token: String) {
    let mut ap = AttachParams::default().stderr(false);
    if let Some(c) = container {
        ap = ap.container(c);
    }
    let run = async {
        let cmd = vec!["sh", "-c", EXEC_HANGUP, "sh", token.as_str()];
        if let Ok(mut p) = api.exec(&pod, cmd, &ap).await {
            // Read the one-line count so the stream is not left unread, then
            // wait for the exit status.
            if let Some(mut o) = p.stdout() {
                let mut sink = Vec::new();
                let _ = o.read_to_end(&mut sink).await;
            }
            let _ = p.join().await;
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), run).await;
}

#[allow(clippy::too_many_arguments)]
pub async fn exec(
    client: Client,
    ns: &str,
    pod: &str,
    container: Option<String>,
    rows: u16,
    cols: u16,
    out: mpsc::Sender<Vec<u8>>,
    mut input: mpsc::Receiver<Vec<u8>>,
    mut resize: mpsc::Receiver<(u16, u16)>,
) -> Result<(), String> {
    check("namespace", ns)?;
    check("pod", pod)?;
    let mut ap = AttachParams::interactive_tty();
    if let Some(c) = container {
        ap = ap.container(c);
    }
    // The 1 KiB defaults make a stall far too easy to reach; 64 KiB matches
    // what a terminal paste or a burst of output actually is.
    ap.max_stdin_buf_size = Some(64 * 1024);
    ap.max_stdout_buf_size = Some(64 * 1024);
    let api = Api::<Pod>::namespaced(client, ns);
    let token = session_token();
    let hangup = (api.clone(), pod.to_string(), ap.container.clone(), token.clone());
    let mut proc = api
        .exec(pod, vec!["/bin/sh".to_string(), "-c".to_string(), shell_command(&token)], &ap)
        .await
        .map_err(|e| e.to_string())?;
    let mut stdout = proc.stdout().ok_or("exec: no stdout")?;
    let mut stdin = proc.stdin().ok_or("exec: no stdin")?;
    // A futures channel: `try_send` is non-blocking, and a dropped resize is
    // harmless because the next one supersedes it.
    let mut size = proc.terminal_size();
    if let Some(s) = size.as_mut() {
        let _ = s.try_send(kube::api::TerminalSize { width: cols, height: rows });
    }
    // Separate reader and writer tasks — see the Docker exec for why one loop
    // deadlocks. kube's attach buffers are 1 KiB by default, so here a paste of
    // a couple of KiB with output arriving was enough to freeze the terminal.
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                keys = input.recv() => match keys {
                    Some(k) => {
                        if stdin.write_all(&k).await.is_err() {
                            break;
                        }
                    }
                    None => break, // session closed
                },
                Some((r, c)) = resize.recv() => {
                    if let Some(s) = size.as_mut() {
                        let _ = s.try_send(kube::api::TerminalSize { width: c, height: r });
                    }
                }
            }
        }
        let _ = stdin.shutdown().await;
    });
    tokio::spawn(async move {
        let mut buf = vec![0u8; 32 * 1024];
        let closed_by_user = loop {
            tokio::select! {
                n = stdout.read(&mut buf) => match n {
                    Ok(0) | Err(_) => break false,
                    Ok(n) => {
                        if out.send(buf[..n].to_vec()).await.is_err() {
                            break true;
                        }
                    }
                },
                _ = out.closed() => break true,
            }
        };
        writer.abort();
        // Dropping the process aborts kube's websocket loop.
        drop(stdout);
        drop(proc);
        if closed_by_user {
            let (api, pod, container, token) = hangup;
            hang_up(api, pod, container, token).await;
        }
    });
    Ok(())
}

/// Used by the namespace picker when the informer is not allowed to list
/// namespaces (namespace-scoped RBAC): best effort, never an error.
pub async fn namespaces(client: Client) -> Vec<String> {
    match Api::<Namespace>::all(client).list(&Default::default()).await {
        Ok(l) => l.items.iter().map(|n| n.name_any()).collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateWaiting, ContainerStatus, PodSpec, PodStatus,
    };

    fn pod(phase: &str, waiting: Option<&str>, ready: bool) -> Pod {
        Pod {
            metadata: kube::api::ObjectMeta {
                uid: Some("u1".into()),
                name: Some("web-1".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: vec![Default::default()],
                ..Default::default()
            }),
            status: Some(PodStatus {
                phase: Some(phase.into()),
                container_statuses: Some(vec![ContainerStatus {
                    ready,
                    restart_count: 3,
                    state: waiting.map(|r| ContainerState {
                        waiting: Some(ContainerStateWaiting {
                            reason: Some(r.into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
        }
    }

    fn api_error(code: u16) -> kube::Error {
        kube::Error::Api(kube::core::Status::failure("x", "y").with_code(code).boxed())
    }

    #[test]
    fn only_credential_failures_read_as_unauthorized() {
        assert!(is_unauthorized(&err(api_error(401))));
        // 403 is RBAC: the credentials are fine, the permission is missing.
        // Rebuilding the client would not help, so it must not trigger one.
        for code in [403, 404, 409, 500] {
            assert!(!is_unauthorized(&err(api_error(code))), "{code}");
        }
        let upgrade = |c: u16| {
            kube::Error::UpgradeConnection(kube::client::UpgradeConnectionError::ProtocolSwitch(
                http::StatusCode::from_u16(c).unwrap(),
            ))
        };
        assert!(is_unauthorized(&err(upgrade(401))), "exec/attach upgrade refused");
        assert!(!is_unauthorized(&err(upgrade(403))));
    }

    #[test]
    fn a_watch_rejected_for_credentials_is_recognised() {
        assert!(watch_rejected_credentials(&watcher::Error::InitialListFailed(api_error(401))));
        assert!(watch_rejected_credentials(&watcher::Error::WatchFailed(api_error(401))));
        assert!(watch_rejected_credentials(&watcher::Error::WatchError(
            kube::core::Status::failure("x", "Unauthorized").with_code(401).boxed()
        )));
        assert!(!watch_rejected_credentials(&watcher::Error::WatchFailed(api_error(410))));
        assert!(!watch_rejected_credentials(&watcher::Error::NoResourceVersion));
    }

    #[test]
    fn the_exec_wrapper_records_the_session_for_the_hangup() {
        let t = session_token();
        assert_eq!(t.len(), 16);
        assert_ne!(t, session_token(), "tokens must not repeat");
        let cmd = shell_command(&t);
        assert!(cmd.contains(&format!("OPSPILOT_SESSION={t}.$$")), "{cmd}");
        assert!(EXEC_HANGUP.contains("OPSPILOT_SESSION="), "shared script not included");
    }

    #[test]
    fn status_matches_kubectl_not_the_phase() {
        assert_eq!(pod_status(&pod("Running", Some("CrashLoopBackOff"), false)), ("CrashLoopBackOff".into(), "bad"));
        assert_eq!(pod_status(&pod("Pending", Some("ContainerCreating"), false)), ("ContainerCreating".into(), "warn"));
        assert_eq!(pod_status(&pod("Running", None, true)), ("Running".into(), "live"));
        assert_eq!(pod_status(&pod("Running", None, false)).1, "warn");
        assert_eq!(pod_status(&pod("Succeeded", None, false)), ("Completed".into(), "idle"));
        let r = pod_row(&pod("Running", None, true));
        assert_eq!(r.cells[0], "1/1");
        assert_eq!(r.cells[2], "3");
    }

    #[test]
    fn names_are_validated_as_dns_subdomains() {
        for ok in ["web", "web-1", "kube-system", "a.b.c"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "Web", "-web", "web-", "a/b", "a b", "../x"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_740_700_800), "2025-02-28T00:00:00Z");
    }

    #[test]
    fn a_relist_swaps_in_atomically() {
        let st = Mutex::new(State::default());
        let mut synced = false;
        apply(&st, Event::InitApply(pod("Running", None, true)), pod_row, &mut synced);
        assert_eq!(st.lock().unwrap().rows.len(), 1);
        // Relist begins: the old rows stay visible until InitDone.
        apply(&st, Event::<Pod>::Init, pod_row, &mut synced);
        assert_eq!(st.lock().unwrap().rows.len(), 1);
        apply(&st, Event::<Pod>::InitDone, pod_row, &mut synced);
        // The relist returned nothing: the pod is gone now, not before.
        assert!(st.lock().unwrap().rows.is_empty());
        assert!(synced);
    }
}
