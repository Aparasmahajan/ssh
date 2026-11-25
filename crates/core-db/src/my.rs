//! MySQL / MariaDB.
//!
//! Rows are pulled from the result stream one chunk at a time. Between chunks
//! the stream is simply not polled, so TCP flow control holds the server back —
//! the same "fetch N, wait for scroll" behaviour as the Postgres cursor, without
//! a server-side cursor (MySQL's need prepared statements and a read-only
//! transaction to be useful).

use std::time::Duration;

use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder, Value};

use crate::model::{Cell, ColumnMeta, Kind, Sink, Wait, CHUNK};

pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: Option<String>,
}

pub fn opts(t: &Target) -> Opts {
    OptsBuilder::default()
        .ip_or_hostname(t.host.clone())
        .tcp_port(t.port)
        .user(Some(t.user.clone()))
        .pass(t.password.clone())
        .db_name(t.database.clone().filter(|d| !d.is_empty()))
        // A desktop client must never silently prefer a local socket over the
        // host the user typed.
        .prefer_socket(false)
        // Without keepalive a dropped network hangs a query (and the session
        // lock it holds) until the OS gives up, which can be hours.
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .tcp_nodelay(true)
        .into()
}

pub async fn connect(opts: Opts) -> Result<Conn, String> {
    tokio::time::timeout(Duration::from_secs(10), Conn::new(opts))
        .await
        .map_err(|_| "connect timed out after 10 s".to_string())?
        .map_err(|e| e.to_string())
}

/// `KILL QUERY` from a second connection — the only way MySQL stops a running
/// statement.
pub async fn kill(opts: Opts, connection_id: u32) -> Result<(), String> {
    let mut c = connect(opts).await?;
    c.query_drop(format!("KILL QUERY {connection_id}"))
        .await
        .map_err(|e| e.to_string())?;
    let _ = c.disconnect().await;
    Ok(())
}

pub fn kind_of(t: ColumnType, flags: ColumnFlags) -> Kind {
    use ColumnType::*;
    match t {
        // An unsigned BIGINT can exceed i64; keep it exact as numeric text.
        MYSQL_TYPE_LONGLONG if flags.contains(ColumnFlags::UNSIGNED_FLAG) => Kind::Numeric,
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_LONG | MYSQL_TYPE_INT24
        | MYSQL_TYPE_LONGLONG | MYSQL_TYPE_YEAR => Kind::Int,
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => Kind::Float,
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => Kind::Numeric,
        _ => Kind::Text,
    }
}

fn type_name(t: ColumnType) -> String {
    format!("{t:?}").trim_start_matches("MYSQL_TYPE_").to_lowercase()
}

/// Text-protocol values arrive as bytes; binary columns that are not UTF-8 are
/// shown as hex rather than as replacement characters.
pub fn cell(v: Value, binary: bool) -> Cell {
    match v {
        Value::NULL => Cell::Null,
        Value::Int(i) => Cell::Int(i),
        Value::UInt(u) => i64::try_from(u).map_or_else(|_| Cell::Text(u.to_string()), Cell::Int),
        Value::Float(f) => Cell::Float(f as f64),
        Value::Double(f) => Cell::Float(f),
        Value::Bytes(b) => match String::from_utf8(b) {
            Ok(s) => Cell::Text(s),
            Err(e) => {
                let b = e.into_bytes();
                let hex: String = b.iter().take(64).map(|x| format!("{x:02x}")).collect();
                let _ = binary;
                Cell::Text(if b.len() > 64 {
                    format!("0x{hex}… ({} bytes)", b.len())
                } else {
                    format!("0x{hex}")
                })
            }
        },
        Value::Date(y, mo, d, h, mi, s, us) => Cell::Text(if (h, mi, s, us) == (0, 0, 0, 0) {
            format!("{y:04}-{mo:02}-{d:02}")
        } else if us == 0 {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        } else {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
        }),
        Value::Time(neg, days, h, mi, s, us) => {
            let hours = days * 24 + h as u32;
            let sign = if neg { "-" } else { "" };
            Cell::Text(if us == 0 {
                format!("{sign}{hours:02}:{mi:02}:{s:02}")
            } else {
                format!("{sign}{hours:02}:{mi:02}:{s:02}.{us:06}")
            })
        }
    }
}

/// How long to let a result drain after an early stop before giving up on the
/// connection. MySQL has no way to abandon a result other than reading it to
/// the end or closing the socket.
const DRAIN_LIMIT: Duration = Duration::from_secs(10);

/// Run `sql`. Returns `true` when the connection must be replaced: an early
/// stop whose remaining result could not be drained within [`DRAIN_LIMIT`].
pub async fn run(conn: &mut Conn, sql: &str, mut sink: Sink, opts: &Opts) -> bool {
    let id = conn.id();
    let mut result = match conn.query_iter(sql).await {
        Ok(r) => r,
        Err(e) => {
            sink.fail(e.to_string());
            return false;
        }
    };

    let columns = result.columns();
    let Some(columns) = columns.filter(|c| !c.is_empty()) else {
        let affected = result.affected_rows();
        let _ = result.drop_result().await;
        sink.finish(Some(affected), None);
        return false;
    };

    let binary: Vec<bool> = columns
        .iter()
        .map(|c| c.flags().contains(ColumnFlags::BINARY_FLAG))
        .collect();
    sink.set_columns(
        columns
            .iter()
            .map(|c| ColumnMeta {
                name: c.name_str().into_owned(),
                type_name: type_name(c.column_type()),
                kind: kind_of(c.column_type(), c.flags()),
            })
            .collect(),
    );

    let mut in_chunk = 0usize;
    let mut stopped_early = false;
    loop {
        match result.next().await {
            Ok(Some(row)) => {
                let vals = row.unwrap_raw();
                sink.push_row(
                    vals.into_iter()
                        .enumerate()
                        .map(|(i, v)| v.map_or(Cell::Null, |v| cell(v, binary.get(i).copied().unwrap_or(false)))),
                );
                in_chunk += 1;
                if in_chunk >= CHUNK {
                    in_chunk = 0;
                    if sink.full() {
                        sink.note_truncated("result capped (row or memory limit); add a LIMIT or WHERE");
                        stopped_early = true;
                        break;
                    }
                    match sink.wait_for_demand().await {
                        Wait::More => {}
                        Wait::Cancelled => {
                            sink.note_truncated("cancelled");
                            stopped_early = true;
                            break;
                        }
                        Wait::Idle => {
                            // The server holds a metadata lock while it still has
                            // rows to send; release it.
                            sink.note_truncated("cursor closed after 60 s idle — re-run to load more");
                            stopped_early = true;
                            break;
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                sink.fail(e.to_string());
                return false;
            }
        }
    }

    if !stopped_early {
        // Discard any further result sets of a multi-statement query.
        let _ = result.drop_result().await;
        sink.finish(None, None);
        return false;
    }

    // Stopped early. Report first, so the UI is right however long the rest
    // takes; then stop the server sending, and drain what is in flight.
    sink.finish(None, None);
    let _ = tokio::time::timeout(Duration::from_secs(5), kill(opts.clone(), id)).await;
    // Drained, or ended by the KILL ("Query execution was interrupted"):
    // either way the connection is usable again. Still streaming after the
    // limit means KILL could not reach the right thread (a proxy rewrote the
    // id, or no new connection was allowed): the only way out is to close it.
    tokio::time::timeout(DRAIN_LIMIT, result.drop_result()).await.is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_render_like_the_mysql_client() {
        assert_eq!(cell(Value::Date(2025, 3, 7, 0, 0, 0, 0), false), Cell::Text("2025-03-07".into()));
        assert_eq!(
            cell(Value::Date(2025, 3, 7, 9, 5, 1, 0), false),
            Cell::Text("2025-03-07 09:05:01".into())
        );
        assert_eq!(cell(Value::Time(true, 1, 2, 3, 4, 0), false), Cell::Text("-26:03:04".into()));
        assert_eq!(cell(Value::UInt(u64::MAX), false), Cell::Text(u64::MAX.to_string()));
        assert_eq!(cell(Value::Bytes(vec![0xff, 0x00]), true), Cell::Text("0xff00".into()));
        assert_eq!(cell(Value::Bytes(b"hi".to_vec()), false), Cell::Text("hi".into()));
    }

    #[test]
    fn unsigned_bigint_stays_exact() {
        assert_eq!(kind_of(ColumnType::MYSQL_TYPE_LONGLONG, ColumnFlags::UNSIGNED_FLAG), Kind::Numeric);
        assert_eq!(kind_of(ColumnType::MYSQL_TYPE_LONGLONG, ColumnFlags::empty()), Kind::Int);
        assert_eq!(kind_of(ColumnType::MYSQL_TYPE_NEWDECIMAL, ColumnFlags::empty()), Kind::Numeric);
    }
}
