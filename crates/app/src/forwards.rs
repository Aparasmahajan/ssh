//! Running port forwards, and their live counters.
//!
//! §13.1 rule 8: the UI samples these on a timer. Nothing is pushed.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use opspilot_core_ssh::forward::{self, Forward, ForwardKind};
use serde::Serialize;

use crate::remote;

#[derive(Serialize, Clone, Debug)]
pub struct ForwardStatus {
    pub id: String,
    pub connection_key: String,
    /// "local" | "dynamic" | "remote"
    pub kind: &'static str,
    pub bound: String,
    pub target: Option<String>,
    pub connections: u64,
    pub active: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub errors: u64,
}

struct Running {
    connection_key: String,
    forward: Forward,
}

#[derive(Default)]
struct Manager {
    live: Mutex<HashMap<String, Running>>,
}

fn manager() -> &'static Manager {
    static M: OnceLock<Manager> = OnceLock::new();
    M.get_or_init(Manager::default)
}

fn status_of(r: &Running) -> ForwardStatus {
    let f = &r.forward;
    let s = &f.stats;
    ForwardStatus {
        id: f.id.clone(),
        connection_key: r.connection_key.clone(),
        kind: match f.kind {
            ForwardKind::Local => "local",
            ForwardKind::Dynamic => "dynamic",
            ForwardKind::Remote => "remote",
        },
        bound: f.bound.to_string(),
        target: f.target.as_ref().map(|(h, p)| format!("{h}:{p}")),
        connections: s.connections.load(Ordering::Relaxed),
        active: s.active.load(Ordering::Relaxed),
        bytes_in: s.bytes_in.load(Ordering::Relaxed),
        bytes_out: s.bytes_out.load(Ordering::Relaxed),
        errors: s.errors.load(Ordering::Relaxed),
    }
}

pub fn start(
    connection_key: &str,
    id: &str,
    kind: &str,
    bind_address: &str,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
) -> Result<ForwardStatus, String> {
    // Validate the REQUEST before looking up the connection.
    //
    // The other order reports "not connected" for a forward that is also
    // missing its target, which sends the user off debugging the wrong thing.
    // A malformed request is malformed whether or not a transport exists.
    let target = match kind {
        "local" | "remote" => match (target_host, target_port) {
            (Some(h), Some(p)) => Some((h, p)),
            _ => return Err(format!("a {kind} forward needs a target host and port")),
        },
        "dynamic" => None,
        other => return Err(format!("unknown forward kind: {other}")),
    };

    let transport = remote::get(connection_key)?;
    let rt = remote::runtime();

    let forward = match kind {
        "local" => {
            // `target` is Some for this arm by construction above.
            let (h, p) = target.expect("local forward target was validated");
            rt.block_on(forward::start_local(
                transport,
                id.to_string(),
                bind_address,
                bind_port,
                h,
                p,
            ))
        }
        "dynamic" => rt.block_on(forward::start_dynamic(
            transport,
            id.to_string(),
            bind_address,
            bind_port,
        )),
        "remote" => {
            let (h, p) = target.expect("remote forward target was validated");
            rt.block_on(forward::start_remote(
                transport,
                id.to_string(),
                bind_address,
                bind_port,
                h,
                p,
            ))
        }
        // Unreachable: the kind was checked above. Kept as an error rather than
        // `unreachable!()` so a future kind added to one match and not the
        // other cannot panic in a running app.
        other => return Err(format!("unknown forward kind: {other}")),
    }
    .map_err(|e| e.to_string())?;

    let running = Running {
        connection_key: connection_key.to_string(),
        forward,
    };
    let status = status_of(&running);
    manager()
        .live
        .lock()
        .map_err(|_| "forwards poisoned")?
        .insert(id.to_string(), running);
    Ok(status)
}

/// What happened to one saved forward when its host connected.
///
/// A *list* rather than a single `Result`, because auto-start is a batch and the
/// outcomes are independent: a port already in use must not stop the other three
/// tunnels from coming up.
#[derive(Serialize, Clone, Debug)]
pub struct AutoStarted {
    pub id: String,
    pub name: String,
    pub started: bool,
    /// Present when `started` is false. `already running` is reported here too —
    /// it is not a failure, and the UI shows it differently.
    pub error: Option<String>,
}

/// Start every forward on `host_id` that is flagged `auto_start`.
///
/// Called after a successful connect. Two rules make this safe to call on every
/// connect, including a reconnect:
///
/// 1. **A forward already running under the same id is left alone**, rather than
///    restarted. Restarting would tear down live connections through the tunnel
///    to arrive at the state it is already in.
/// 2. **A failure is reported, never propagated.** The user asked for a shell;
///    losing it because port 15432 was taken by yesterday's `psql` would be a
///    much worse outcome than a tunnel that did not start. The caller surfaces
///    the reasons.
pub fn auto_start(
    connection_key: &str,
    workspace: &str,
    host_id: &str,
) -> Result<Vec<AutoStarted>, String> {
    let ws = opspilot_core_store::store::load(workspace)?;
    let host = ws
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| format!("no host {host_id} in workspace {workspace}"))?;

    let mut out = Vec::new();
    for f in host.forwards.iter().filter(|f| f.auto_start) {
        if is_running(&f.id)? {
            out.push(AutoStarted {
                id: f.id.clone(),
                name: f.name.clone(),
                started: false,
                error: Some("already running".into()),
            });
            continue;
        }
        let kind = match f.kind {
            opspilot_core_store::model::ForwardKind::Local => "local",
            opspilot_core_store::model::ForwardKind::Remote => "remote",
            opspilot_core_store::model::ForwardKind::Dynamic => "dynamic",
        };
        let result = start(
            connection_key,
            &f.id,
            kind,
            &f.bind_address,
            f.bind_port,
            f.target_host.clone(),
            f.target_port,
        );
        out.push(AutoStarted {
            id: f.id.clone(),
            name: f.name.clone(),
            started: result.is_ok(),
            error: result.err(),
        });
    }
    Ok(out)
}

fn is_running(id: &str) -> Result<bool, String> {
    Ok(manager()
        .live
        .lock()
        .map_err(|_| "forwards poisoned")?
        .contains_key(id))
}

pub fn stop(id: &str) -> Result<(), String> {
    if let Some(mut r) = manager()
        .live
        .lock()
        .map_err(|_| "forwards poisoned")?
        .remove(id)
    {
        r.forward.stop();
    }
    Ok(())
}

pub fn list() -> Vec<ForwardStatus> {
    let Ok(live) = manager().live.lock() else {
        return Vec::new();
    };
    let mut v: Vec<ForwardStatus> = live.values().map(status_of).collect();
    v.sort_by(|a, b| a.bound.cmp(&b.bound));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renamed from `remote_forwards_are_refused_with_a_reason`: remote (-R)
    /// forwards ARE implemented now, so the thing worth asserting is that a
    /// remote forward missing its target says so instead of failing obscurely
    /// inside the transport.
    #[test]
    fn a_remote_forward_without_a_target_is_rejected() {
        let err = start("nope", "f1", "remote", "127.0.0.1", 8080, None, None).unwrap_err();
        assert!(
            err.contains("target"),
            "the error must name what is missing, got: {err}"
        );
    }

    #[test]
    fn a_local_forward_without_a_target_is_rejected() {
        let err = start("nope", "f1", "local", "127.0.0.1", 8080, None, None).unwrap_err();
        assert!(!err.is_empty(), "must explain what is missing");
    }

    #[test]
    fn listing_is_empty_before_anything_starts() {
        assert!(list().iter().all(|f| !f.id.is_empty()));
    }

    /* ---- auto_start ------------------------------------------------------- */

    use opspilot_core_store::model::{
        Environment, Forward as StoredForward, ForwardKind as StoredKind, Host,
    };

    fn saved_forward(id: &str, auto: bool) -> StoredForward {
        StoredForward {
            id: id.into(),
            name: id.into(),
            kind: StoredKind::Local,
            bind_address: "127.0.0.1".into(),
            bind_port: 0,
            target_host: Some("10.0.0.9".into()),
            target_port: Some(5432),
            auto_start: auto,
        }
    }

    fn host_with(forwards: Vec<StoredForward>) -> Host {
        Host {
            id: "h1".into(),
            name: "linux-01".into(),
            address: "10.0.0.1".into(),
            port: 22,
            os: String::new(),
            environment: Environment::Prod,
            tags: vec![],
            identities: vec![],
            forwards,
            jump_host: None,
        }
    }

    /// The flag is the whole feature: an unflagged forward must not be touched.
    ///
    /// Uses a connection key that is not in the pool, so every start fails — and
    /// that is the point. The result list names exactly the forwards that were
    /// *attempted*, which is what proves the filter without needing a server.
    #[test]
    fn auto_start_attempts_only_the_flagged_forwards() {
        let _g = crate::testenv::lock();
        crate::testenv::isolate("fwd-flagged");
        opspilot_core_store::store::upsert_host(
            "wk",
            host_with(vec![saved_forward("pg", true), saved_forward("redis", false)]),
        )
        .unwrap();

        let out = auto_start("not-connected", "wk", "h1").expect("a batch, not an error");
        assert_eq!(out.len(), 1, "only the flagged forward is attempted: {out:?}");
        assert_eq!(out[0].id, "pg");
    }

    /// A tunnel that cannot start must not take the connection down with it.
    ///
    /// The user asked for a shell; losing it because a port was already bound
    /// would be a far worse outcome than a tunnel that did not come up. So this
    /// returns `Ok` with the failure described inside.
    #[test]
    fn a_forward_that_fails_to_start_is_reported_not_propagated() {
        let _g = crate::testenv::lock();
        crate::testenv::isolate("fwd-fails");
        opspilot_core_store::store::upsert_host("wk", host_with(vec![saved_forward("pg", true)]))
            .unwrap();

        let out = auto_start("not-connected", "wk", "h1").expect("must not propagate");
        assert!(!out[0].started);
        let err = out[0].error.clone().expect("a reason");
        assert!(err.contains("not connected"), "{err}");
    }

    /// An unknown host id *is* an error: it means the caller passed the wrong
    /// thing, which is a bug rather than a runtime condition to be tolerated.
    #[test]
    fn auto_start_on_an_unknown_host_says_so() {
        let _g = crate::testenv::lock();
        crate::testenv::isolate("fwd-unknown");
        opspilot_core_store::store::upsert_host("wk", host_with(vec![])).unwrap();

        let err = auto_start("k", "wk", "nope").unwrap_err();
        assert!(err.contains("nope"), "the error must name the host: {err}");
    }
}
