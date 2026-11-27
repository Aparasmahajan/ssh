//! SQLite, on a dedicated thread.
//!
//! rusqlite is synchronous and its `Connection` is not `Sync`, so each SQLite
//! session owns one OS thread and receives work over a channel. That keeps
//! blocking file I/O off the async runtime (CLAUDE.md §3: "No blocking std::fs
//! inside a tokio task"). The statement is stepped a chunk at a time and the
//! thread waits for demand between chunks, exactly like the network engines.
//!
//! SQLite is dynamically typed — a column declared INTEGER can hold text — so
//! numeric columns are carried as numeric *text*: nothing is lost if a row
//! disagrees with its declaration, and they still sort by value.

use std::path::PathBuf;
use std::sync::mpsc;

use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};

use crate::model::{Cell, ColumnMeta, Kind, Sink, Wait, CHUNK};

pub enum Job {
    Run(String, Sink),
}

/// A connection living on its own thread. Dropping this ends the thread.
pub struct Lite {
    /// Bounded (§13.1 rule 7). A session runs one query at a time; the depth
    /// only absorbs a Run pressed while the previous one is winding down.
    tx: mpsc::SyncSender<Job>,
    /// Stops a statement mid-execution. Without it, Cancel only took effect
    /// between 500-row chunks, so a long UPDATE or CREATE INDEX ran to the end.
    interrupt: rusqlite::InterruptHandle,
}

impl Lite {
    /// Opens (never creates — a typo in a path must not leave an empty
    /// database behind) and starts the worker thread.
    pub fn open(path: PathBuf) -> Result<Lite, String> {
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        // Another process writing must mean "wait a moment", not an instant
        // "database is locked".
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        let interrupt = conn.get_interrupt_handle();
        let (tx, rx) = mpsc::sync_channel::<Job>(4);
        std::thread::Builder::new()
            .name("opspilot-sqlite".into())
            .spawn(move || {
                while let Ok(Job::Run(sql, sink)) = rx.recv() {
                    // Superseded or closed while queued: never execute it.
                    if sink.query().is_cancelled() {
                        sink.fail("cancelled before it started".into());
                        continue;
                    }
                    run(&conn, &sql, sink);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Lite { tx, interrupt })
    }

    pub fn submit(&self, sql: String, sink: Sink) -> Result<(), String> {
        self.tx.try_send(Job::Run(sql, sink)).map_err(|e| match e {
            mpsc::TrySendError::Full(_) => "sqlite busy: previous queries still stopping".to_string(),
            mpsc::TrySendError::Disconnected(_) => "sqlite worker stopped".to_string(),
        })
    }

    /// Abort whatever statement is executing right now.
    pub fn interrupt(&self) {
        self.interrupt.interrupt();
    }
}

pub fn kind_of(decl: Option<&str>) -> Kind {
    // SQLite's own affinity rules (§3.1 of its datatype docs), simplified.
    let d = decl.unwrap_or("").to_ascii_uppercase();
    if d.contains("INT") || d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") || d.contains("NUM")
        || d.contains("DEC")
    {
        Kind::Numeric
    } else {
        Kind::Text
    }
}

fn cell(v: ValueRef<'_>) -> Cell {
    match v {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(i) => Cell::Text(i.to_string()),
        ValueRef::Real(f) => Cell::Text(f.to_string()),
        ValueRef::Text(t) => Cell::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            let hex: String = b.iter().take(64).map(|x| format!("{x:02x}")).collect();
            Cell::Text(if b.len() > 64 {
                format!("x'{hex}'… ({} bytes)", b.len())
            } else {
                format!("x'{hex}'")
            })
        }
    }
}

/// Run `sql`, which may be several statements.
///
/// Statement by statement, through rusqlite's `Batch`, each executed before the
/// next is prepared. Two simpler designs were tried and both were wrong:
///
/// - `Connection::prepare` does NOT reject a script: it compiles the first
///   statement and silently drops the rest, so "create …; insert …;" reported
///   success having only created the table.
/// - Counting statements first means preparing statement 2 before statement 1
///   has run, which fails for any script whose DDL the later statements use
///   ("no such table").
///
/// A statement that returns rows is held back until the next one has been
/// prepared: if it turns out to be the last row-returning statement, its rows
/// are what the grid shows; otherwise it is run and its rows discarded.
pub fn run(conn: &Connection, sql: &str, mut sink: Sink) {
    let mut batch = rusqlite::Batch::new(conn, sql);
    let mut held: Option<rusqlite::Statement<'_>> = None;
    let mut count = 0usize;
    let mut last_changes: Option<u64> = None;
    loop {
        let next = match batch.next() {
            Ok(n) => n,
            Err(e) => return sink.fail(e.to_string()),
        };
        let Some(mut stmt) = next else { break };
        count += 1;
        // Cancel between statements too, not only between chunks.
        if sink.query().is_cancelled() {
            return sink.fail(format!("cancelled before statement {count}"));
        }
        // An earlier SELECT that is not the last: run it now, in order.
        if let Some(mut prev) = held.take() {
            let mut rows = prev.raw_query();
            loop {
                if sink.query().is_cancelled() {
                    return sink.fail("cancelled".into());
                }
                match rows.next() {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => return sink.fail(e.to_string()),
                }
            }
        }
        if stmt.column_count() == 0 {
            if let Err(e) = stmt.raw_execute() {
                return sink.fail(format!("statement {count}: {e}"));
            }
            last_changes = Some(conn.changes());
        } else {
            held = Some(stmt);
        }
    }

    let script = (count > 1).then(|| format!("script executed ({count} statements)"));
    let Some(mut stmt) = held else {
        return sink.finish(if count > 1 { None } else { last_changes }, script);
    };

    let columns: Vec<ColumnMeta> = stmt
        .columns()
        .iter()
        .map(|c| ColumnMeta {
            name: c.name().to_string(),
            type_name: c.decl_type().unwrap_or("").to_lowercase(),
            kind: kind_of(c.decl_type()),
        })
        .collect();
    let n = columns.len();
    sink.set_columns(columns);

    let mut rows = stmt.raw_query();
    let mut in_chunk = 0usize;
    loop {
        match rows.next() {
            Ok(Some(row)) => {
                sink.push_row((0..n).map(|i| row.get_ref(i).map(cell).unwrap_or(Cell::Null)));
                in_chunk += 1;
                if in_chunk >= CHUNK {
                    in_chunk = 0;
                    if sink.full() {
                        sink.note_truncated("result capped (row or memory limit); add a LIMIT or WHERE");
                        break;
                    }
                    match sink.wait_for_demand_blocking() {
                        Wait::More => {}
                        Wait::Cancelled => {
                            sink.note_truncated("cancelled");
                            break;
                        }
                        // A parked statement holds a read transaction, which
                        // blocks other writers (rollback journal) and the WAL
                        // checkpoint.
                        Wait::Idle => {
                            sink.note_truncated("cursor closed after 60 s idle — re-run to load more");
                            break;
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => return sink.fail(e.to_string()),
        }
    }
    sink.finish(None, script);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Query;

    fn db() -> (PathBuf, Lite) {
        let p = std::env::temp_dir().join(format!("opspilot-lite-{}-{:?}.db", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_file(&p);
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "create table t(id integer, name text, score real, blob blob);
             insert into t values (2,'b',9.5,x'00ff'),(10,'a',10.25,null),(1,'c',null,null);",
        )
        .unwrap();
        drop(c);
        let l = Lite::open(p.clone()).unwrap();
        (p, l)
    }

    fn wait(q: &Query) -> crate::model::Window {
        for _ in 0..200 {
            let w = q.window(0, 100);
            if w.done {
                return w;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("query never finished");
    }

    #[test]
    fn select_streams_rows_and_sorts_integers_numerically() {
        let (_p, l) = db();
        let q = Query::new();
        l.submit("select id, name, score, blob from t".into(), q.sink()).unwrap();
        let w = wait(&q);
        assert_eq!(w.loaded, 3);
        assert_eq!(w.columns[0].kind, Kind::Numeric);
        assert_eq!(w.rows[0][3].as_deref(), Some("x'00ff'"));
        q.set_view(Some((0, false)), String::new());
        let w = q.window(0, 10);
        let ids: Vec<_> = w.rows.iter().map(|r| r[0].clone().unwrap()).collect();
        assert_eq!(ids, ["1", "2", "10"], "numeric, not text, order");
    }

    #[test]
    fn dml_reports_changes_and_scripts_run() {
        let (_p, l) = db();
        let q = Query::new();
        l.submit("update t set name = 'z' where id < 5".into(), q.sink()).unwrap();
        assert_eq!(wait(&q).affected, Some(2));
        let q = Query::new();
        l.submit("create table u(x); insert into u values (1);".into(), q.sink()).unwrap();
        assert_eq!(wait(&q).message.as_deref(), Some("script executed (2 statements)"));
        // The second statement really ran.
        let q = Query::new();
        l.submit("select count(*) from u".into(), q.sink()).unwrap();
        assert_eq!(wait(&q).rows[0][0].as_deref(), Some("1"));
        // A script ending in SELECT shows that SELECT's rows.
        let q = Query::new();
        l.submit("insert into u values (2); select x from u order by x".into(), q.sink()).unwrap();
        let w = wait(&q);
        assert_eq!(w.loaded, 2);
        assert_eq!(w.rows[1][0].as_deref(), Some("2"));
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        let (_p, l) = db();
        let q = Query::new();
        l.submit("select nope from t".into(), q.sink()).unwrap();
        let w = wait(&q);
        assert!(w.error.unwrap().contains("nope"));
    }

    #[test]
    fn a_missing_file_is_not_created() {
        let p = std::env::temp_dir().join("opspilot-definitely-missing.db");
        let _ = std::fs::remove_file(&p);
        assert!(Lite::open(p.clone()).is_err());
        assert!(!p.exists());
    }
}
