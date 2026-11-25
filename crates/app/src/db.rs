//! Database commands: saved profiles, open sessions, and result windows.
//!
//! The webview never holds a result set. It asks for the rows it is about to
//! paint (`db_window`), and the rest stays in Arrow columns here (§11).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use opspilot_core_db::model::Window;
use opspilot_core_db::session::{Engine, Session, Target};
use opspilot_core_store::databases::{self, DbProfile};
use serde::Serialize;

use crate::remote::runtime;

struct Open {
    session: Session,
    /// Bumped by every run, so a window that belongs to a superseded query
    /// can be recognised and dropped by the UI.
    seq: AtomicU64,
}

fn sessions() -> &'static Mutex<HashMap<u32, Arc<Open>>> {
    static S: OnceLock<Mutex<HashMap<u32, Arc<Open>>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn get(id: u32) -> Result<Arc<Open>, String> {
    sessions()
        .lock()
        .map_err(|_| "db registry poisoned")?
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no database session {id}"))
}

/// Run on the network runtime from any async context (see docker.rs).
async fn on_runtime<T: Send + 'static>(
    f: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    runtime()
        .spawn(f)
        .await
        .map_err(|e| format!("database task failed: {e}"))?
}

/* ---- Profiles -------------------------------------------------------------- */

/// File I/O (and an fsync on save) — never on the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

pub async fn profiles() -> Result<Vec<DbProfile>, String> {
    blocking(databases::load).await
}

pub async fn save_profile(p: DbProfile) -> Result<Vec<DbProfile>, String> {
    blocking(move || databases::upsert(p)).await
}

pub async fn delete_profile(id: String) -> Result<Vec<DbProfile>, String> {
    blocking(move || databases::delete(&id)).await
}

/* ---- Sessions -------------------------------------------------------------- */

#[derive(Serialize)]
pub struct Opened {
    pub session: u32,
    pub label: String,
    pub engine: Engine,
}

fn engine_of(s: &str) -> Result<Engine, String> {
    match s {
        "postgres" => Ok(Engine::Postgres),
        "mysql" => Ok(Engine::Mysql),
        "sqlite" => Ok(Engine::Sqlite),
        other => Err(format!("unknown engine `{other}`")),
    }
}

/// Connect using a saved profile. `password` is the one typed for this
/// connect, used only if the profile has no vault reference.
pub async fn open(profile_id: String, password: Option<String>) -> Result<Opened, String> {
    let p = blocking(databases::load)
        .await?
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("no saved connection `{profile_id}`"))?;
    let engine = engine_of(&p.engine)?;

    // Resolved here, natively; the secret never crosses IPC (§18.14).
    let password = match p.password_ref.as_deref() {
        Some(r) if !r.is_empty() => {
            if crate::vault::needs_unlock(r) {
                return Err(format!("unlock the vault to use {r}"));
            }
            crate::vault::resolve(r)?
        }
        _ => password.filter(|s| !s.is_empty()),
    };
    let path = p
        .path
        .as_deref()
        .map(|s| PathBuf::from(opspilot_core_store::paths::resolve(s)));
    let target = Target {
        engine,
        host: if p.host.is_empty() { "127.0.0.1".into() } else { p.host.clone() },
        port: if p.port == 0 { engine.default_port() } else { p.port },
        user: p.user.clone(),
        password,
        database: p.database.clone(),
        path,
    };
    let session = on_runtime(Session::open(target)).await?;
    let label = format!("{} · {}", p.name, session.label);
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    sessions()
        .lock()
        .map_err(|_| "db registry poisoned")?
        .insert(
            id,
            Arc::new(Open {
                session,
                seq: AtomicU64::new(0),
            }),
        );
    Ok(Opened {
        session: id,
        label,
        engine,
    })
}

/// Close one session: cancel, abort its task, disconnect — on the network
/// runtime, which mysql_async needs for a clean COM_QUIT.
pub async fn close(id: u32) -> Result<(), String> {
    let s = sessions().lock().map_err(|_| "db registry poisoned")?.remove(&id);
    if let Some(s) = s {
        shutdown(s).await;
    }
    Ok(())
}

async fn shutdown(s: Arc<Open>) {
    // Normally the only reference; if a window call still holds one, the
    // last drop disconnects instead.
    match Arc::try_unwrap(s) {
        Ok(open) => {
            let _ = runtime().spawn(open.session.shutdown()).await;
        }
        // A window call still holds it: stop the query now; the connection
        // closes when that call drops the last reference.
        Err(shared) => {
            let _ = runtime().spawn(async move { shared.session.cancel() }).await;
        }
    }
}

/// Close every session. The UI calls this when it (re)starts: after a webview
/// reload no component remembers the old sessions, and without this each
/// reload would leave its connections — and their open transactions — behind.
pub async fn close_all() -> Result<(), String> {
    let all: Vec<Arc<Open>> = sessions()
        .lock()
        .map_err(|_| "db registry poisoned")?
        .drain()
        .map(|(_, s)| s)
        .collect();
    for s in all {
        shutdown(s).await;
    }
    Ok(())
}

/// Start a query; returns its sequence number.
pub async fn run(id: u32, sql: String) -> Result<u64, String> {
    let s = get(id)?;
    if sql.trim().is_empty() {
        return Err("nothing to run".into());
    }
    runtime()
        .spawn(async move {
            // Replace the current query BEFORE publishing the new sequence
            // number, so a window call in between never labels the old
            // query's rows with the new number.
            s.session.run(sql);
            s.seq.fetch_add(1, Ordering::AcqRel) + 1
        })
        .await
        .map_err(|e| e.to_string())
}

pub async fn cancel(id: u32) -> Result<(), String> {
    let s = get(id)?;
    runtime()
        .spawn(async move { s.session.cancel() })
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct WindowView {
    pub seq: u64,
    #[serde(flatten)]
    pub window: Window,
}

/// Rows `[offset, offset + limit)` of the current result's view. Async and
/// on a blocking thread: the first window after a sort sorts the whole result,
/// which must never run on the main thread.
pub async fn window(id: u32, offset: usize, limit: usize) -> Result<Option<WindowView>, String> {
    let s = get(id)?;
    let limit = limit.min(1000);
    tokio::task::spawn_blocking(move || {
        let seq = s.seq.load(Ordering::Acquire);
        s.session
            .current()
            .map(|q| WindowView { seq, window: q.window(offset, limit) })
    })
    .await
    .map_err(|e| e.to_string())
}

pub async fn set_view(id: u32, sort: Option<(usize, bool)>, filter: String) -> Result<(), String> {
    let s = get(id)?;
    tokio::task::spawn_blocking(move || {
        if let Some(q) = s.session.current() {
            q.set_view(sort, filter);
        }
    })
    .await
    .map_err(|e| e.to_string())
}
