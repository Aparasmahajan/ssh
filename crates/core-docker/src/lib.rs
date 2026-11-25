//! Docker Engine client for OpsPilot.
//!
//! CLAUDE.md §10: "Docker: **events API + streaming stats**, never polling. Log
//! streaming reuses the terminal ring buffer and coalescing rules verbatim."
//!
//! - [`endpoint`] finds the daemon the way the `docker` CLI does.
//! - [`Watch`] keeps a live container list: one list, then the events stream;
//!   a burst of events (compose up) costs one re-list, not one per event. Stats
//!   stream per running container into the same state. The UI samples it by
//!   generation counter (§13.1 rule 8) — nothing is pushed per event.
//! - [`exec`] and [`logs`] feed plain tokio channels, which the app wires into
//!   an ordinary terminal session.

pub mod endpoint;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::models::{ContainerStatsResponse, ContainerSummary, ExecConfig};
use bollard::query_parameters::{
    EventsOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
    ResizeExecOptions, StatsOptions, StopContainerOptions,
};
use bollard::exec::{StartExecOptions, StartExecResults};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use bollard::Docker as Api;
pub use endpoint::Endpoint;

/* ---- Connecting ----------------------------------------------------------- */

/// Seconds a single request may take. Generous: `stop` waits for the
/// container's own grace period (10 s by default) before the daemon answers.
const TIMEOUT_S: u64 = 120;

pub fn connect(ep: &Endpoint) -> Result<Api, String> {
    let v = bollard::API_DEFAULT_VERSION;
    let h = ep.host.as_str();
    let res = if h.starts_with("unix://") {
        Api::connect_with_unix(h, TIMEOUT_S, v)
    } else if h.starts_with("tcp://") || h.starts_with("http://") {
        Api::connect_with_http(h, TIMEOUT_S, v)
    } else if h.starts_with("npipe://") {
        #[cfg(windows)]
        {
            Api::connect_with_named_pipe(h, TIMEOUT_S, v)
        }
        #[cfg(not(windows))]
        {
            return Err(format!("{h}: named pipes exist only on Windows"));
        }
    } else {
        // ssh:// works in the CLI by spawning `ssh`; it is not supported here
        // yet, and saying so beats a confusing connection error.
        return Err(format!(
            "unsupported Docker endpoint `{h}` (from {}): only unix://, tcp:// and npipe:// are supported",
            ep.source
        ));
    };
    res.map_err(|e| format!("{h}: {e}"))
}

/// Docker's own name grammar, `[a-zA-Z0-9][a-zA-Z0-9_.-]*`, which also covers
/// hex ids.
///
/// This is a security check, not tidiness: bollard formats the id straight
/// into the request path (`/containers/{id}/stop`). An id containing `/` or `?`
/// would address a different API endpoint entirely.
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        && id.len() <= 128
}

fn check(id: &str) -> Result<(), String> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(format!("invalid container id `{}`", id.escape_debug()))
    }
}

/* ---- Live state ----------------------------------------------------------- */

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// created | running | paused | restarting | exited | removing | dead
    pub state: String,
    /// The daemon's human status: "Up 3 hours (healthy)".
    pub status: String,
    /// "0.0.0.0:8080→80/tcp, 443/tcp"
    pub ports: String,
    /// Unix seconds.
    pub created: i64,
    /// `com.docker.compose.project`, so the UI can group a stack.
    pub project: Option<String>,
    pub cpu_pct: Option<f64>,
    pub mem_bytes: Option<u64>,
    pub mem_limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Stat {
    cpu_pct: f64,
    mem: u64,
    limit: u64,
}

#[derive(Default)]
struct State {
    containers: Vec<Container>,
    stats: HashMap<String, Stat>,
    error: Option<String>,
    connected: bool,
    version: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub connected: bool,
    pub error: Option<String>,
    pub version: Option<String>,
    pub containers: Vec<Container>,
}

/// A live view of one daemon. Dropping it stops every task it started.
pub struct Watch {
    generation: Arc<AtomicU64>,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Stats streams by container id; aborted when dropped, so aborting the watch
/// task (which owns this) stops every stream with it.
#[derive(Default)]
struct StatTasks(HashMap<String, JoinHandle<()>>);

impl Drop for StatTasks {
    fn drop(&mut self) {
        for (_, t) in self.0.drain() {
            t.abort();
        }
    }
}

/// Events that arrive within this window of each other cost one re-list.
/// `docker compose up` of ten services emits ~60 events in a second.
const COALESCE: Duration = Duration::from_millis(60);

impl Watch {
    pub fn start(api: Api, rt: &tokio::runtime::Handle) -> Watch {
        let generation = Arc::new(AtomicU64::new(1));
        let state = Arc::new(Mutex::new(State::default()));
        let task = rt.spawn(run(api, Arc::clone(&state), Arc::clone(&generation)));
        Watch {
            generation,
            state,
            task,
        }
    }

    /// `None` when nothing changed since `since`: an idle daemon costs the UI
    /// one atomic load per sample.
    pub fn snapshot(&self, since: u64) -> Option<Snapshot> {
        let generation = self.generation.load(Ordering::Acquire);
        if generation == since {
            return None;
        }
        let g = self.state.lock().ok()?;
        let containers = g
            .containers
            .iter()
            .map(|c| {
                let mut c = c.clone();
                if let Some(s) = g.stats.get(&c.id) {
                    c.cpu_pct = Some(s.cpu_pct);
                    c.mem_bytes = Some(s.mem);
                    c.mem_limit = Some(s.limit);
                }
                c
            })
            .collect();
        Some(Snapshot {
            generation,
            connected: g.connected,
            error: g.error.clone(),
            version: g.version.clone(),
            containers,
        })
    }
}

async fn run(api: Api, state: Arc<Mutex<State>>, generation: Arc<AtomicU64>) {
    let bump = || {
        generation.fetch_add(1, Ordering::Release);
    };
    let mut attempt: u32 = 0;
    loop {
        let mut stats = StatTasks::default();
        match watch_once(&api, &state, &generation, &mut stats).await {
            Ok(()) => attempt = 0,
            Err(e) => {
                let mut was_connected = false;
                if let Ok(mut g) = state.lock() {
                    was_connected = g.connected;
                    g.connected = false;
                    g.error = Some(e);
                    g.stats.clear();
                }
                // A connection that worked and then dropped (daemon restart)
                // starts the backoff over; only consecutive failures grow it.
                if was_connected {
                    attempt = 0;
                }
                bump();
            }
        }
        drop(stats);
        // §8.3: exponential backoff with full jitter. A daemon restart
        // reconnects within a second; a stopped Docker Desktop is retried
        // every 30 s without spinning.
        attempt = attempt.saturating_add(1);
        let cap = 30_000u64;
        let base = 500u64.saturating_mul(1 << attempt.min(6));
        let ms = jitter(base.min(cap));
        tokio::time::sleep(Duration::from_millis(ms.max(250))).await;
    }
}

/// `rand(0..max)` without a dependency: the low bits of the clock are noise
/// enough to de-synchronise reconnects, which is all §8.3 needs.
fn jitter(max: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    if max == 0 {
        0
    } else {
        nanos % max
    }
}

/// One connected lifetime: list, then follow events until the stream ends.
async fn watch_once(
    api: &Api,
    state: &Arc<Mutex<State>>,
    generation: &Arc<AtomicU64>,
    stats: &mut StatTasks,
) -> Result<(), String> {
    let version = api.version().await.map_err(|e| e.to_string())?;
    if let Ok(mut g) = state.lock() {
        g.version = version.version;
    }

    // Subscribe BEFORE listing, so nothing that happens in between is lost.
    let mut filters = HashMap::new();
    filters.insert("type".to_string(), vec!["container".to_string()]);
    // Lifecycle only. `type=container` alone includes exec_create/exec_start/
    // exec_die, which healthchecks emit every few seconds per container — on
    // a host with many healthchecked containers that was a re-list every
    // coalescing window, forever.
    filters.insert(
        "event".to_string(),
        [
            "create", "start", "restart", "stop", "die", "kill", "destroy", "pause", "unpause", "rename",
            "health_status", "oom", "update",
        ]
        .iter()
        .map(|e| e.to_string())
        .collect(),
    );
    let mut events = api.events(Some(EventsOptions {
        since: None,
        until: None,
        filters: Some(filters),
    }));

    relist(api, state, generation, stats).await?;

    loop {
        match events.next().await {
            Some(Ok(_)) => {
                // Drain whatever else arrives within the coalescing window.
                let deadline = tokio::time::Instant::now() + COALESCE;
                while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.next()).await {
                    ev.map_err(|e| e.to_string())?;
                }
                relist(api, state, generation, stats).await?;
            }
            Some(Err(e)) => return Err(e.to_string()),
            None => return Err("docker event stream ended".into()),
        }
    }
}

async fn relist(
    api: &Api,
    state: &Arc<Mutex<State>>,
    generation: &Arc<AtomicU64>,
    stats: &mut StatTasks,
) -> Result<(), String> {
    let list = api
        .list_containers(Some(ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
        .map_err(|e| e.to_string())?;
    let mut containers: Vec<Container> = list.into_iter().filter_map(summarize).collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));

    // Reconcile stats streams with the running set. O(containers).
    let running: std::collections::HashSet<&str> = containers
        .iter()
        .filter(|c| c.state == "running")
        .map(|c| c.id.as_str())
        .collect();
    stats.0.retain(|id, task| {
        let keep = running.contains(id.as_str()) && !task.is_finished();
        if !keep {
            task.abort();
        }
        keep
    });
    for id in &running {
        if !stats.0.contains_key(*id) {
            let t = tokio::spawn(stream_stats(
                api.clone(),
                id.to_string(),
                Arc::clone(state),
                Arc::clone(generation),
            ));
            stats.0.insert(id.to_string(), t);
        }
    }

    if let Ok(mut g) = state.lock() {
        g.stats.retain(|id, _| running.contains(id.as_str()));
        g.containers = containers;
        g.connected = true;
        g.error = None;
    }
    generation.fetch_add(1, Ordering::Release);
    Ok(())
}

fn summarize(c: ContainerSummary) -> Option<Container> {
    let id = c.id?;
    let name = c
        .names
        .and_then(|n| n.into_iter().next())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_else(|| id.chars().take(12).collect());
    let ports = c
        .ports
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let proto = p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into());
            match p.public_port {
                Some(pub_port) => {
                    // Wildcard binds (both families) read as just the port; a
                    // specific address is kept, bracketed if IPv6.
                    let ip = p.ip.unwrap_or_default();
                    let host = match ip.as_str() {
                        "" | "0.0.0.0" | "::" => String::new(),
                        v6 if v6.contains(':') => format!("[{v6}]:"),
                        v4 => format!("{v4}:"),
                    };
                    format!("{host}{pub_port}→{}/{proto}", p.private_port)
                }
                None => format!("{}/{}", p.private_port, proto),
            }
        })
        // IPv4 and IPv6 bindings of the same port read as a duplicate.
        .fold(Vec::<String>::new(), |mut acc, s| {
            if !acc.contains(&s) {
                acc.push(s);
            }
            acc
        })
        .join(", ");
    let project = c
        .labels
        .as_ref()
        .and_then(|l| l.get("com.docker.compose.project").cloned());
    Some(Container {
        id,
        name,
        image: c.image.unwrap_or_default(),
        state: c
            .state
            .map(|s| s.to_string())
            .unwrap_or_default()
            .to_lowercase(),
        status: c.status.unwrap_or_default(),
        ports,
        created: c.created.unwrap_or(0),
        project,
        cpu_pct: None,
        mem_bytes: None,
        mem_limit: None,
    })
}

async fn stream_stats(
    api: Api,
    id: String,
    state: Arc<Mutex<State>>,
    generation: Arc<AtomicU64>,
) {
    let mut s = api.stats(
        &id,
        Some(StatsOptions {
            stream: true,
            one_shot: false,
        }),
    );
    while let Some(Ok(r)) = s.next().await {
        if let Some(stat) = compute_stat(&r) {
            if let Ok(mut g) = state.lock() {
                g.stats.insert(id.clone(), stat);
            }
            generation.fetch_add(1, Ordering::Release);
        }
    }
}

/// The same arithmetic `docker stats` uses.
///
/// CPU: container CPU time delta over host CPU time delta, times the CPU count.
/// Memory: usage minus page cache (`inactive_file` on cgroup v2, `cache` on
/// v1) — raw `usage` counts reclaimable cache and makes every container look
/// like it is leaking.
fn compute_stat(r: &ContainerStatsResponse) -> Option<Stat> {
    let cpu = r.cpu_stats.as_ref()?;
    let pre = r.precpu_stats.as_ref();
    let total = cpu.cpu_usage.as_ref()?.total_usage? as f64;
    let pre_total = pre
        .and_then(|p| p.cpu_usage.as_ref())
        .and_then(|u| u.total_usage)
        .unwrap_or(0) as f64;
    let sys = cpu.system_cpu_usage.unwrap_or(0) as f64;
    let pre_sys = pre.and_then(|p| p.system_cpu_usage).unwrap_or(0) as f64;
    let cpus = cpu.online_cpus.unwrap_or(1).max(1) as f64;
    let (dc, ds) = (total - pre_total, sys - pre_sys);
    let cpu_pct = if dc > 0.0 && ds > 0.0 {
        dc / ds * cpus * 100.0
    } else {
        0.0
    };

    let mem = r.memory_stats.as_ref()?;
    let usage = mem.usage.unwrap_or(0);
    let cache = mem
        .stats
        .as_ref()
        .and_then(|s| s.get("inactive_file").or_else(|| s.get("cache")).copied())
        .unwrap_or(0);
    Some(Stat {
        cpu_pct,
        mem: usage.saturating_sub(cache),
        limit: mem.limit.unwrap_or(0),
    })
}

/* ---- Actions -------------------------------------------------------------- */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    /// Remove; `force` also kills a running container.
    Remove { force: bool },
}

impl Action {
    pub fn parse(s: &str) -> Option<Action> {
        Some(match s {
            "start" => Action::Start,
            "stop" => Action::Stop,
            "restart" => Action::Restart,
            "pause" => Action::Pause,
            "unpause" => Action::Unpause,
            "remove" => Action::Remove { force: false },
            "remove-force" => Action::Remove { force: true },
            _ => return None,
        })
    }
}

pub async fn act(api: &Api, id: &str, action: Action) -> Result<(), String> {
    check(id)?;
    let r = match action {
        Action::Start => api.start_container(id, None).await,
        Action::Stop => {
            api.stop_container(id, None::<StopContainerOptions>).await
        }
        Action::Restart => api.restart_container(id, None).await,
        Action::Pause => api.pause_container(id).await,
        Action::Unpause => api.unpause_container(id).await,
        Action::Remove { force } => {
            api.remove_container(
                id,
                Some(RemoveContainerOptions {
                    force,
                    v: false,
                    link: false,
                }),
            )
            .await
        }
    };
    r.map_err(|e| e.to_string())
}

/* ---- Exec and logs: bytes for a terminal session --------------------------- */

/// Prefer bash, fall back to sh — the same thing `docker exec -it x sh` users
/// end up typing by hand. Distroless images have neither, and the daemon's
/// error says so.
const SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";

/// Sends SIGHUP to what an exec session left running once its tab closes.
/// See the script's header; shared with core-kube so there is one copy.
pub const EXEC_HANGUP: &str = include_str!("exec_hangup.sh");

/// A fresh token naming one exec session. Unique, not secret: the worst a
/// process that forged it could do is get itself hung up.
pub fn session_token() -> String {
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

/// The exec command line: record `<token>.<own pid>` for [`EXEC_HANGUP`], then
/// become the shell. `exec` keeps the pid, so the recorded pid is the shell's,
/// and with a TTY it is also its session id.
pub fn shell_command(token: &str) -> String {
    format!("export OPSPILOT_SESSION={token}.$$; {SHELL}")
}

/// Hang up whatever the exec session `token` left behind, if it is still
/// running. Best effort and detached: a container without `sh` has nothing
/// this could do anyway.
async fn hang_up(api: &Api, container: &str, exec_id: &str, token: &str) {
    // The common case is a shell the user `exit`ed: nothing left, and no
    // second exec to spend.
    if let Ok(i) = api.inspect_exec(exec_id).await {
        if i.running == Some(false) {
            return;
        }
    }
    let Ok(created) = api
        .create_exec(
            container,
            ExecConfig {
                cmd: Some(vec![
                    "sh".into(),
                    "-c".into(),
                    EXEC_HANGUP.into(),
                    "sh".into(),
                    token.into(),
                ]),
                ..Default::default()
            },
        )
        .await
    else {
        return;
    };
    let _ = api
        .start_exec(
            &created.id,
            Some(StartExecOptions {
                detach: true,
                tty: false,
                output_capacity: None,
            }),
        )
        .await;
}

/// Start an interactive shell in a running container and pump it through the
/// given channels until either side closes.
///
/// Returns once the exec is running, so "container is not running" reaches the
/// UI as an error rather than as a terminal that silently never prints.
pub async fn exec(
    api: &Api,
    id: &str,
    rows: u16,
    cols: u16,
    out: mpsc::Sender<Vec<u8>>,
    mut input: mpsc::Receiver<Vec<u8>>,
    mut resize: mpsc::Receiver<(u16, u16)>,
) -> Result<(), String> {
    check(id)?;
    let token = session_token();
    let created = api
        .create_exec(
            id,
            ExecConfig {
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(true),
                console_size: Some(vec![rows as usize, cols as usize]),
                // Same rule as a local session (CLAUDE.md §18.16): the shell
                // must know what it is talking to.
                env: Some(vec![
                    "TERM=xterm-256color".into(),
                    "COLORTERM=truecolor".into(),
                ]),
                cmd: Some(vec!["/bin/sh".into(), "-c".into(), shell_command(&token)]),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let exec_id = created.id;

    let started = api
        .start_exec(
            &exec_id,
            Some(StartExecOptions {
                detach: false,
                tty: true,
                output_capacity: None,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    let StartExecResults::Attached {
        mut output,
        input: mut stdin,
    } = started
    else {
        return Err("exec started detached".into());
    };

    // Two tasks, not one. A single loop that both reads output and writes
    // input deadlocks on a large paste: while `write_all` waits for the remote
    // to drain stdin, nothing reads stdout, so the remote (blocked writing its
    // echo) never drains stdin. Closing the tab could not break it either —
    // the loop never got back to notice.
    let api = api.clone();
    let hangup = (api.clone(), id.to_string(), exec_id.clone(), token);
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                keys = input.recv() => match keys {
                    Some(k) => {
                        if stdin.write_all(&k).await.is_err() || stdin.flush().await.is_err() {
                            break;
                        }
                    }
                    None => break, // session closed
                },
                Some((r, c)) = resize.recv() => {
                    let _ = api
                        .resize_exec(&exec_id, ResizeExecOptions { h: r as i32, w: c as i32 })
                        .await;
                }
            }
        }
        // EOF on stdin ends an interactive shell, which ends the exec.
        let _ = stdin.shutdown().await;
    });
    tokio::spawn(async move {
        let closed_by_user = loop {
            tokio::select! {
                chunk = output.next() => match chunk {
                    Some(Ok(o)) => {
                        if out.send(o.into_bytes().to_vec()).await.is_err() {
                            break true;
                        }
                    }
                    _ => break false, // the exec ended
                },
                // Session closed while the remote is silent.
                _ = out.closed() => break true,
            }
        };
        // Whichever way output ended, the writer has nothing left to serve —
        // and may be stuck mid-write on a stalled remote.
        writer.abort();
        drop(output);
        if closed_by_user {
            let (api, container, exec_id, token) = hangup;
            hang_up(&api, &container, &exec_id, &token).await;
        }
    });
    Ok(())
}

/// Follow a container's logs: the last `tail` lines, then everything new.
pub fn logs(api: &Api, id: &str, tail: u32, out: mpsc::Sender<Vec<u8>>) -> Result<(), String> {
    check(id)?;
    let mut stream = api.logs(
        id,
        Some(LogsOptions {
            follow: true,
            stdout: true,
            stderr: true,
            tail: tail.to_string(),
            ..Default::default()
        }),
    );
    tokio::spawn(async move {
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(o)) => {
                        if out.send(o.into_bytes().to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        let _ = out.send(format!("\r\n[log stream error: {e}]\r\n").into_bytes()).await;
                        break;
                    }
                    None => {
                        let _ = out.send(b"\r\n[container stopped: log stream ended]\r\n".to_vec()).await;
                        break;
                    }
                },
                // Session closed while the container is quiet: stop now, not at
                // the next log line, which may never come.
                _ = out.closed() => break,
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerCpuStats, ContainerCpuUsage, ContainerMemoryStats};

    #[test]
    fn ids_that_could_reach_another_endpoint_are_rejected() {
        for ok in ["web", "my_app-1", "a1b2c3d4e5f6", "compose.web.1"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "../../images/x", "a/b", "a?force=1", "-rm", ".hidden", "a b", "x\n"] {
            assert!(!valid_id(bad), "{bad:?} must be rejected");
        }
        assert!(!valid_id(&"a".repeat(129)));
    }

    #[test]
    fn actions_parse_from_their_wire_names() {
        assert_eq!(Action::parse("remove-force"), Some(Action::Remove { force: true }));
        assert_eq!(Action::parse("kill"), None);
    }

    fn stats(total: u64, pre_total: u64, sys: u64, pre_sys: u64, cpus: u32) -> ContainerStatsResponse {
        let mut s = HashMap::new();
        s.insert("inactive_file".to_string(), 100u64);
        ContainerStatsResponse {
            cpu_stats: Some(ContainerCpuStats {
                cpu_usage: Some(ContainerCpuUsage {
                    total_usage: Some(total),
                    ..Default::default()
                }),
                system_cpu_usage: Some(sys),
                online_cpus: Some(cpus),
                ..Default::default()
            }),
            precpu_stats: Some(ContainerCpuStats {
                cpu_usage: Some(ContainerCpuUsage {
                    total_usage: Some(pre_total),
                    ..Default::default()
                }),
                system_cpu_usage: Some(pre_sys),
                ..Default::default()
            }),
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(1100),
                limit: Some(4096),
                stats: Some(s),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn cpu_and_memory_match_docker_stats_arithmetic() {
        // Half of one CPU's share on a 4-CPU host = 50% * ... : 50/400 * 4 * 100.
        let s = compute_stat(&stats(150, 100, 1400, 1000, 4)).unwrap();
        assert!((s.cpu_pct - 50.0).abs() < 1e-9, "{}", s.cpu_pct);
        // Page cache is not counted as used memory.
        assert_eq!(s.mem, 1000);
        assert_eq!(s.limit, 4096);
    }

    #[test]
    fn the_first_sample_has_no_delta_and_reads_zero_not_garbage() {
        let s = compute_stat(&stats(150, 0, 1400, 0, 4)).unwrap();
        assert!(s.cpu_pct.is_finite());
        let s = compute_stat(&stats(100, 100, 1000, 1000, 4)).unwrap();
        assert_eq!(s.cpu_pct, 0.0);
    }

    #[test]
    fn ports_are_deduplicated_across_ipv4_and_ipv6() {
        use bollard::models::{PortSummary, PortSummaryTypeEnum};
        let c = ContainerSummary {
            id: Some("abc".into()),
            names: Some(vec!["/web".into()]),
            ports: Some(vec![
                PortSummary {
                    ip: Some("0.0.0.0".into()),
                    private_port: 80,
                    public_port: Some(8080),
                    typ: Some(PortSummaryTypeEnum::TCP),
                },
                PortSummary {
                    ip: Some("::".into()),
                    private_port: 80,
                    public_port: Some(8080),
                    typ: Some(PortSummaryTypeEnum::TCP),
                },
            ]),
            ..Default::default()
        };
        let s = summarize(c).unwrap();
        assert_eq!(s.name, "web");
        assert_eq!(s.ports.matches("8080").count(), 1, "{}", s.ports);
    }
}
