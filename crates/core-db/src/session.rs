//! One open database connection, and the query running on it.
//!
//! A session is a connection, not a pool: transactions, temp tables and
//! `SET` variables persist between queries the way they do in psql. Running a
//! new query cancels the one still streaming, which is what a user who hits
//! Run twice means.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::lite::Lite;
use crate::model::Query;
use crate::{my, pg};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Sqlite,
}

impl Engine {
    pub fn default_port(self) -> u16 {
        match self {
            Engine::Postgres => 5432,
            Engine::Mysql => 3306,
            Engine::Sqlite => 0,
        }
    }
}

/// Everything needed to open a connection, secrets already resolved.
pub struct Target {
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
    /// SQLite only: an absolute, already-resolved path.
    pub path: Option<PathBuf>,
}

enum Conn {
    Pg {
        client: Arc<tokio::sync::Mutex<tokio_postgres::Client>>,
        cancel: tokio_postgres::CancelToken,
    },
    My {
        /// `None` once the connection had to be abandoned and could not be
        /// re-established; the next query reports it.
        conn: Arc<tokio::sync::Mutex<Option<mysql_async::Conn>>>,
        opts: mysql_async::Opts,
        /// The server thread id, for `KILL QUERY`. Changes on reconnect.
        id: Arc<AtomicU32>,
    },
    Lite(Lite),
}

struct Current {
    query: Query,
    /// The task running it, so closing the session can stop it outright
    /// rather than leave it holding a connection nobody will use again.
    task: Option<tokio::task::AbortHandle>,
}

pub struct Session {
    conn: Conn,
    current: Mutex<Option<Current>>,
    /// "postgres 16.4 · testdb@127.0.0.1:55432"
    pub label: String,
}

impl Session {
    /// Connect. Call on a tokio runtime.
    pub async fn open(t: Target) -> Result<Session, String> {
        let label = match t.engine {
            Engine::Sqlite => format!(
                "sqlite · {}",
                t.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
            ),
            e => format!("{e:?} · {}@{}:{}", t.database, t.host, t.port).to_lowercase(),
        };
        let conn = match t.engine {
            Engine::Postgres => {
                let (client, cancel) = pg::connect(&pg::Target {
                    host: t.host,
                    port: t.port,
                    user: t.user,
                    password: t.password,
                    database: t.database,
                })
                .await?;
                Conn::Pg {
                    client: Arc::new(tokio::sync::Mutex::new(client)),
                    cancel,
                }
            }
            Engine::Mysql => {
                let opts = my::opts(&my::Target {
                    host: t.host,
                    port: t.port,
                    user: t.user,
                    password: t.password,
                    database: Some(t.database),
                });
                let conn = my::connect(opts.clone()).await?;
                let id = Arc::new(AtomicU32::new(conn.id()));
                Conn::My {
                    conn: Arc::new(tokio::sync::Mutex::new(Some(conn))),
                    opts,
                    id,
                }
            }
            Engine::Sqlite => {
                let path = t.path.ok_or("sqlite: no file path")?;
                // Opening a file is blocking I/O; keep it off the async worker.
                let lite = tokio::task::spawn_blocking(move || Lite::open(path))
                    .await
                    .map_err(|e| e.to_string())??;
                Conn::Lite(lite)
            }
        };
        Ok(Session {
            conn,
            current: Mutex::new(None),
            label,
        })
    }

    /// Start `sql`. Returns at once; rows stream into the returned query.
    /// Call on a tokio runtime.
    pub fn run(&self, sql: String) -> Query {
        self.cancel();
        let q = Query::new();
        let sink = q.sink();
        let task = match &self.conn {
            Conn::Pg { client, .. } => {
                let client = Arc::clone(client);
                Some(tokio::spawn(async move {
                    // Waits for the previous query to let go of the client,
                    // which its cancel has just told it to do.
                    let mut c = client.lock().await;
                    // Superseded while waiting (Run pressed again, or the
                    // session closed): never execute it.
                    if sink.query().is_cancelled() {
                        return sink.fail("superseded before it started".into());
                    }
                    pg::run(&mut c, &sql, sink).await;
                }))
            }
            Conn::My { conn, opts, id } => {
                let (conn, opts, id) = (Arc::clone(conn), opts.clone(), Arc::clone(id));
                Some(tokio::spawn(async move {
                    let mut guard = conn.lock().await;
                    if sink.query().is_cancelled() {
                        return sink.fail("superseded before it started".into());
                    }
                    let Some(c) = guard.as_mut() else {
                        return sink.fail("connection lost; reconnect".into());
                    };
                    if my::run(c, &sql, sink, &opts).await {
                        // The rest of the result could not be drained.
                        // Abandon the connection OFF the runtime: dropped on
                        // a runtime, mysql_async spawns a cleanup that drains
                        // the pending result — forever, for a huge one.
                        if let Some(old) = guard.take() {
                            let _ = std::thread::Builder::new()
                                .name("opspilot-mysql-drop".into())
                                .spawn(move || drop(old));
                        }
                        if let Ok(fresh) = my::connect(opts).await {
                            id.store(fresh.id(), Ordering::Release);
                            *guard = Some(fresh);
                        }
                    }
                }))
            }
            Conn::Lite(l) => {
                if let Err(e) = l.submit(sql, sink) {
                    // The sink was moved into the failed send; report via a
                    // fresh one on the same query.
                    q.sink().fail(e);
                }
                None
            }
        };
        if let Ok(mut cur) = self.current.lock() {
            *cur = Some(Current {
                query: q.clone(),
                task: task.map(|t| t.abort_handle()),
            });
        }
        q
    }

    pub fn current(&self) -> Option<Query> {
        self.current
            .lock()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.query.clone()))
    }

    /// Stop the current query: stop fetching, and ask the server to stop
    /// executing. A no-op if nothing is running. Never blocks — it reads
    /// atomics only, so it is safe from the IPC thread.
    pub fn cancel(&self) {
        let Some(q) = self.current() else { return };
        if q.is_done() {
            return;
        }
        // A producer parked waiting for the UI has nothing executing on the
        // server. A server-side cancel sent now would arrive after it wakes,
        // finishes, and releases the connection — and kill the NEXT query.
        let executing = !q.is_parked();
        q.cancel();
        if !executing {
            return;
        }
        match &self.conn {
            Conn::Pg { cancel, .. } => {
                let token = cancel.clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        token.cancel_query(tokio_postgres::NoTls),
                    )
                    .await;
                });
            }
            Conn::My { opts, id, .. } => {
                let (opts, id) = (opts.clone(), id.load(Ordering::Acquire));
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), my::kill(opts, id)).await;
                });
            }
            // Stops the statement mid-step; the worker also sees the flag.
            Conn::Lite(l) => l.interrupt(),
        }
    }

    /// Close: stop the query, abort its task, and disconnect cleanly where
    /// that cannot hang. Call on a tokio runtime.
    pub async fn shutdown(self) {
        self.cancel();
        if let Ok(mut cur) = self.current.lock() {
            if let Some(c) = cur.take() {
                if let Some(t) = c.task {
                    t.abort();
                }
            }
        }
        match self.conn {
            // Dropping the client makes its connection task send Terminate.
            Conn::Pg { client, .. } => drop(client),
            Conn::My { conn, .. } => {
                let taken = match tokio::time::timeout(std::time::Duration::from_secs(2), conn.lock()).await {
                    Ok(mut g) => g.take(),
                    Err(_) => None,
                };
                if let Some(c) = taken {
                    // COM_QUIT, bounded. If it cannot finish (a result still
                    // pending), fall back to closing the socket off-runtime.
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), c.disconnect()).await;
                }
            }
            // Dropping the sender ends the worker thread after its job.
            Conn::Lite(l) => drop(l),
        }
    }
}
