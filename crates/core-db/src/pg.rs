//! PostgreSQL: extended protocol, binary format, true cursors.
//!
//! §11: "use the binary format and extended-query pipelining". A query that
//! returns rows is bound to a portal inside a transaction and fetched
//! [`CHUNK`] rows per `Execute` — the server holds the cursor, so a 10 M-row
//! `SELECT` costs one chunk until someone scrolls. Statements that return no
//! rows run directly and report the affected count.
//!
//! Values are decoded from the binary wire format here, by type OID, rather
//! than through chrono/uuid/rust_decimal feature crates: the grid only needs
//! display text, and those crates exist to give you typed Rust values.

use std::error::Error;
use std::time::Duration;

use tokio_postgres::types::{FromSql, Kind as PgKind, Type};
use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, Config, NoTls, Statement, Transaction};

use crate::model::{Cell, ColumnMeta, Kind, Sink, Wait};

pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
}

/// Connect. The connection's I/O task is spawned on the caller's runtime and
/// ends when the client is dropped.
pub async fn connect(t: &Target) -> Result<(Client, tokio_postgres::CancelToken), String> {
    let mut cfg = Config::new();
    // Built field by field, never from a formatted connection string, so a
    // password containing `'` or a space cannot change the meaning of another
    // field.
    cfg.host(&t.host)
        .port(t.port)
        .user(&t.user)
        .dbname(&t.database)
        .application_name("OpsPilot")
        .connect_timeout(Duration::from_secs(10))
        // Default keepalive idle is 2 h: a dropped network would leave a query
        // (and the session lock it holds) hanging for hours.
        .keepalives_idle(Duration::from_secs(30))
        .tcp_user_timeout(Duration::from_secs(60));
    if let Some(p) = &t.password {
        cfg.password(p);
    }
    // `connect_timeout` covers only the TCP connect. A server that accepts and
    // then never answers the startup/auth exchange would hang forever.
    let (client, conn) = tokio::time::timeout(Duration::from_secs(20), cfg.connect(NoTls))
        .await
        .map_err(|_| "connect timed out after 20 s".to_string())?
        .map_err(describe)?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let cancel = client.cancel_token();
    Ok((client, cancel))
}

/// The full error chain: tokio-postgres's Display alone is often just "db
/// error", with the server's actual message one level down.
fn describe(e: tokio_postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        let mut s = format!("{}: {}", db.severity(), db.message());
        if let Some(d) = db.detail() {
            s.push_str(&format!("\n{d}"));
        }
        if let Some(h) = db.hint() {
            s.push_str(&format!("\nhint: {h}"));
        }
        if let Some(p) = db.position() {
            s.push_str(&format!("\n(at {p:?})"));
        }
        return s;
    }
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(&format!(": {inner}"));
        src = inner.source();
    }
    s
}

pub async fn run(client: &mut Client, sql: &str, mut sink: Sink) {
    let stmt = match client.prepare(sql).await {
        Ok(s) => s,
        Err(e) => {
            // Several statements cannot be prepared as one. Run them as a
            // script; there is no single result to show.
            let multi = e
                .as_db_error()
                .map(|d| d.message().contains("multiple commands"))
                .unwrap_or(false);
            if multi {
                match client.batch_execute(sql).await {
                    Ok(()) => sink.finish(None, Some("script executed".into())),
                    Err(e) => sink.fail(describe(e)),
                }
            } else {
                sink.fail(describe(e));
            }
            return;
        }
    };

    if stmt.columns().is_empty() {
        match client.execute(&stmt, &[]).await {
            Ok(n) => sink.finish(Some(n), None),
            Err(e) => sink.fail(describe(e)),
        }
        return;
    }

    sink.set_columns(
        stmt.columns()
            .iter()
            .map(|c| ColumnMeta {
                name: c.name().to_string(),
                type_name: c.type_().name().to_string(),
                kind: kind_of(c.type_()),
            })
            .collect(),
    );

    // A portal needs a transaction. If the user already has one open
    // (`BEGIN; DELETE …;` then a SELECT to check), wrapping our own BEGIN/COMMIT
    // around the cursor would COMMIT their transaction — or ROLLBACK it on any
    // fetch error. So: probe, and use a SAVEPOINT inside theirs instead.
    let in_user_tx = match client.batch_execute("SAVEPOINT opspilot_probe").await {
        Ok(()) => {
            let _ = client.batch_execute("RELEASE SAVEPOINT opspilot_probe").await;
            true
        }
        Err(e) if e.code() == Some(&SqlState::NO_ACTIVE_SQL_TRANSACTION) => false,
        // The user's transaction is in a failed state ("current transaction
        // is aborted"): nothing will run until they ROLLBACK. Say so.
        Err(e) => return sink.fail(describe(e)),
    };

    if in_user_tx {
        // `transaction()` sends a BEGIN, which inside an open transaction is a
        // harmless WARNING. The outer object must never be dropped — its Drop
        // sends ROLLBACK, which would discard the user's work — so it is
        // forgotten; only the savepoint is committed or rolled back.
        let mut outer = match client.transaction().await {
            Ok(t) => t,
            Err(e) => return sink.fail(describe(e)),
        };
        // Scoped so the savepoint's borrow of `outer` ends before `outer` is
        // forgotten.
        let outcome = {
            match outer.savepoint("opspilot_portal").await {
                Ok(sp) => match stream(&sp, &stmt, &mut sink).await {
                    // RELEASE SAVEPOINT: the statement's effects stay in the
                    // user's transaction, for them to commit or roll back.
                    Ok(()) => sp.commit().await.map_err(describe),
                    // Dropping `sp` queues ROLLBACK TO SAVEPOINT: only this
                    // statement is undone; the user's transaction survives.
                    Err(e) => Err(e),
                },
                Err(e) => Err(describe(e)),
            }
        };
        std::mem::forget(outer);
        match outcome {
            Ok(()) => sink.finish(None, None),
            Err(e) => sink.fail(e),
        }
        return;
    }

    let tx = match client.transaction().await {
        Ok(t) => t,
        Err(e) => return sink.fail(describe(e)),
    };
    // Effects persist iff the statement completed without error: committed on
    // success (including an early stop — `INSERT … RETURNING` has already done
    // its work by the first fetch), rolled back on error by the Drop.
    match stream(&tx, &stmt, &mut sink).await {
        Ok(()) => match tx.commit().await {
            Ok(()) => sink.finish(None, None),
            Err(e) => sink.fail(describe(e)),
        },
        Err(e) => sink.fail(e),
    }
}

/// Bind `stmt` to a portal in `tx` and fetch until done, capped, cancelled,
/// or parked past [`PARK_LIMIT`](crate::model::PARK_LIMIT).
async fn stream(tx: &Transaction<'_>, stmt: &Statement, sink: &mut Sink) -> Result<(), String> {
    let portal = tx.bind(stmt, &[]).await.map_err(describe)?;
    loop {
        let ask = sink.batch_size();
        let rows = tx.query_portal(&portal, ask as i32).await.map_err(describe)?;
        let n = rows.len();
        for row in rows {
            sink.push_row((0..row.len()).map(|i| row.try_get::<_, PgCell>(i).map(|c| c.0).unwrap_or(Cell::Null)));
        }
        if n < ask {
            return Ok(());
        }
        if sink.full() {
            sink.note_truncated("result capped (row or memory limit); add a LIMIT or WHERE");
            return Ok(());
        }
        match sink.wait_for_demand().await {
            Wait::More => {}
            Wait::Cancelled => {
                sink.note_truncated("cancelled");
                return Ok(());
            }
            Wait::Idle => {
                // Release the server-side transaction and locks; the rows
                // already fetched stay on screen.
                sink.note_truncated("cursor closed after 60 s idle — re-run to load more");
                return Ok(());
            }
        }
    }
}

pub fn kind_of(t: &Type) -> Kind {
    match *t {
        Type::INT2 | Type::INT4 | Type::INT8 | Type::OID => Kind::Int,
        Type::FLOAT4 | Type::FLOAT8 => Kind::Float,
        Type::BOOL => Kind::Bool,
        Type::NUMERIC => Kind::Numeric,
        _ => Kind::Text,
    }
}

/// Any value, decoded from the binary format by type.
struct PgCell(Cell);

impl<'a> FromSql<'a> for PgCell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(PgCell(decode(ty, raw)))
    }
    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(PgCell(Cell::Null))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}

fn be<const N: usize>(raw: &[u8]) -> Option<[u8; N]> {
    raw.get(..N)?.try_into().ok()
}

/// Binary wire format -> display cell. Unknown types never fail the row; they
/// render as a short description so the rest of the result is still usable.
pub fn decode(ty: &Type, raw: &[u8]) -> Cell {
    let text = |s: &[u8]| Cell::Text(String::from_utf8_lossy(s).into_owned());
    match *ty {
        Type::BOOL => Cell::Bool(raw.first().is_some_and(|b| *b != 0)),
        Type::INT2 => be::<2>(raw).map_or(Cell::Null, |b| Cell::Int(i16::from_be_bytes(b) as i64)),
        Type::INT4 => be::<4>(raw).map_or(Cell::Null, |b| Cell::Int(i32::from_be_bytes(b) as i64)),
        Type::INT8 => be::<8>(raw).map_or(Cell::Null, |b| Cell::Int(i64::from_be_bytes(b))),
        Type::OID => be::<4>(raw).map_or(Cell::Null, |b| Cell::Int(u32::from_be_bytes(b) as i64)),
        Type::FLOAT4 => be::<4>(raw).map_or(Cell::Null, |b| Cell::Float(f32::from_be_bytes(b) as f64)),
        Type::FLOAT8 => be::<8>(raw).map_or(Cell::Null, |b| Cell::Float(f64::from_be_bytes(b))),
        Type::NUMERIC => numeric(raw).map_or(Cell::Null, Cell::Text),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN | Type::JSON | Type::XML
        | Type::CHAR => text(raw),
        // jsonb's binary form is a version byte, then the JSON text.
        Type::JSONB => text(raw.get(1..).unwrap_or_default()),
        Type::UUID => uuid(raw).map_or(Cell::Null, Cell::Text),
        Type::DATE => be::<4>(raw).map_or(Cell::Null, |b| Cell::Text(date(i32::from_be_bytes(b)))),
        Type::TIMESTAMP => be::<8>(raw).map_or(Cell::Null, |b| Cell::Text(timestamp(i64::from_be_bytes(b)))),
        Type::TIMESTAMPTZ => {
            be::<8>(raw).map_or(Cell::Null, |b| Cell::Text(format!("{}+00", timestamp(i64::from_be_bytes(b)))))
        }
        Type::TIME => be::<8>(raw).map_or(Cell::Null, |b| Cell::Text(time_of_day(i64::from_be_bytes(b)))),
        Type::INTERVAL => interval(raw).map_or(Cell::Null, Cell::Text),
        Type::BYTEA => Cell::Text(bytea(raw)),
        Type::INET | Type::CIDR => inet(raw).map_or(Cell::Null, Cell::Text),
        _ => match ty.kind() {
            // Enum values are sent as their label.
            PgKind::Enum(_) => text(raw),
            PgKind::Domain(inner) => decode(inner, raw),
            PgKind::Array(elem) => array(elem, raw).map_or(Cell::Null, Cell::Text),
            // citext, ltree, and other text-like extension types.
            _ if matches!(ty.name(), "citext" | "ltree" | "lquery") => text(raw),
            _ => Cell::Text(format!("<{} · {} bytes>", ty.name(), raw.len())),
        },
    }
}

/// `numeric` binary: ndigits, weight, sign, dscale, then base-10000 digits.
fn numeric(raw: &[u8]) -> Option<String> {
    let rd = |i: usize| -> Option<i16> { Some(i16::from_be_bytes(be::<2>(raw.get(i..)?)?)) };
    let ndigits = rd(0)? as usize;
    let weight = rd(2)? as i32;
    let sign = u16::from_be_bytes(be::<2>(raw.get(4..)?)?);
    let dscale = rd(6)?.max(0) as usize;
    match sign {
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => {}
    }
    let digits: Vec<i16> = (0..ndigits).map(|i| rd(8 + i * 2)).collect::<Option<_>>()?;

    let mut int_part = String::new();
    if weight < 0 {
        int_part.push('0');
    } else {
        for w in 0..=weight {
            let d = digits.get(w as usize).copied().unwrap_or(0);
            if w == 0 {
                int_part.push_str(&d.to_string());
            } else {
                int_part.push_str(&format!("{d:04}"));
            }
        }
    }
    let mut frac = String::new();
    if dscale > 0 {
        // Digit groups after the decimal point start at index weight + 1.
        let mut g = weight + 1;
        while frac.len() < dscale {
            let d = if g < 0 { 0 } else { digits.get(g as usize).copied().unwrap_or(0) };
            frac.push_str(&format!("{d:04}"));
            g += 1;
        }
        frac.truncate(dscale);
    }
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    s.push_str(&int_part);
    if !frac.is_empty() {
        s.push('.');
        s.push_str(&frac);
    }
    Some(s)
}

fn uuid(raw: &[u8]) -> Option<String> {
    let b = raw.get(..16)?;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

/// Days since 1970-01-01 -> (y, m, d). Howard Hinnant's civil_from_days.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Postgres dates and timestamps count from 2000-01-01.
const PG_EPOCH_DAYS: i64 = 10_957;

fn date(days: i32) -> String {
    match days {
        i32::MAX => "infinity".into(),
        i32::MIN => "-infinity".into(),
        _ => {
            let (y, m, d) = civil(days as i64 + PG_EPOCH_DAYS);
            format!("{y:04}-{m:02}-{d:02}")
        }
    }
}

fn timestamp(us: i64) -> String {
    match us {
        i64::MAX => return "infinity".into(),
        i64::MIN => return "-infinity".into(),
        _ => {}
    }
    let day = us.div_euclid(86_400_000_000);
    let rem = us.rem_euclid(86_400_000_000);
    let (y, m, d) = civil(day + PG_EPOCH_DAYS);
    format!("{y:04}-{m:02}-{d:02} {}", time_of_day(rem))
}

fn time_of_day(us: i64) -> String {
    let (s, frac) = (us / 1_000_000, us % 1_000_000);
    let base = format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60);
    if frac == 0 {
        base
    } else {
        let f = format!("{frac:06}");
        format!("{base}.{}", f.trim_end_matches('0'))
    }
}

fn interval(raw: &[u8]) -> Option<String> {
    let us = i64::from_be_bytes(be::<8>(raw)?);
    let days = i32::from_be_bytes(be::<4>(raw.get(8..)?)?);
    let months = i32::from_be_bytes(be::<4>(raw.get(12..)?)?);
    // PostgreSQL 17 encodes ±infinity as all-MIN / all-MAX fields. `abs()` of
    // MIN overflows, so these are handled before any arithmetic.
    if (us, days, months) == (i64::MAX, i32::MAX, i32::MAX) {
        return Some("infinity".into());
    }
    if (us, days, months) == (i64::MIN, i32::MIN, i32::MIN) {
        return Some("-infinity".into());
    }
    let mut parts = Vec::new();
    let (y, mo) = (months / 12, months % 12);
    if y != 0 {
        parts.push(format!("{y} year{}", if y.unsigned_abs() == 1 { "" } else { "s" }));
    }
    if mo != 0 {
        parts.push(format!("{mo} mon{}", if mo.unsigned_abs() == 1 { "" } else { "s" }));
    }
    if days != 0 {
        parts.push(format!("{days} day{}", if days.unsigned_abs() == 1 { "" } else { "s" }));
    }
    if us != 0 || parts.is_empty() {
        let sign = if us < 0 { "-" } else { "" };
        let mag = i64::try_from(us.unsigned_abs()).unwrap_or(i64::MAX);
        parts.push(format!("{sign}{}", time_of_day(mag)));
    }
    Some(parts.join(" "))
}

fn bytea(raw: &[u8]) -> String {
    // Long blobs are elided: a 5 MB image is not something to render in a cell.
    const SHOW: usize = 64;
    let hex: String = raw.iter().take(SHOW).map(|b| format!("{b:02x}")).collect();
    if raw.len() > SHOW {
        format!("\\x{hex}… ({} bytes)", raw.len())
    } else {
        format!("\\x{hex}")
    }
}

fn inet(raw: &[u8]) -> Option<String> {
    // family, bits, is_cidr, nb, address bytes
    let (fam, bits, nb) = (*raw.first()?, *raw.get(1)?, *raw.get(3)? as usize);
    let a = raw.get(4..4 + nb)?;
    let addr = if fam == 2 && nb == 4 {
        std::net::IpAddr::from(<[u8; 4]>::try_from(a).ok()?).to_string()
    } else if nb == 16 {
        std::net::IpAddr::from(<[u8; 16]>::try_from(a).ok()?).to_string()
    } else {
        return None;
    };
    let full = if fam == 2 { 32 } else { 128 };
    Some(if bits == full { addr } else { format!("{addr}/{bits}") })
}

/// One-dimensional arrays as `{a,b,c}`; deeper ones are described, not drawn.
fn array(elem: &Type, raw: &[u8]) -> Option<String> {
    let i32_at = |i: usize| -> Option<i32> { Some(i32::from_be_bytes(be::<4>(raw.get(i..)?)?)) };
    let ndim = i32_at(0)?;
    if ndim == 0 {
        return Some("{}".into());
    }
    if ndim != 1 {
        return Some(format!("<{ndim}-d {} array>", elem.name()));
    }
    let len = i32_at(12)?.max(0) as usize;
    let mut pos = 20;
    // `len` comes off the wire: a malformed header claiming 2^31 elements must
    // not become a 50 GB allocation (which aborts). Each element needs at least
    // its 4-byte length, so this bound is exact for well-formed input.
    let mut out = Vec::with_capacity(len.min(raw.len() / 4));
    for _ in 0..len {
        let n = i32_at(pos)?;
        pos += 4;
        if n < 0 {
            out.push("NULL".to_string());
            continue;
        }
        let v = raw.get(pos..pos + n as usize)?;
        pos += n as usize;
        out.push(match decode(elem, v) {
            Cell::Null => "NULL".into(),
            Cell::Int(i) => i.to_string(),
            Cell::Float(f) => f.to_string(),
            Cell::Bool(b) => (if b { "t" } else { "f" }).into(),
            Cell::Text(s) => {
                if s.is_empty() || s.contains([',', '{', '}', '"', ' ', '\\']) {
                    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    s
                }
            }
        });
    }
    Some(format!("{{{}}}", out.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(ndigits: i16, weight: i16, sign: u16, dscale: i16, digits: &[i16]) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [ndigits, weight] {
            v.extend(x.to_be_bytes());
        }
        v.extend(sign.to_be_bytes());
        v.extend(dscale.to_be_bytes());
        for d in digits {
            v.extend(d.to_be_bytes());
        }
        v
    }

    #[test]
    fn numeric_decodes_exactly() {
        // 12345.678 = [1, 2345, 6780] weight 1, dscale 3
        assert_eq!(numeric(&num(3, 1, 0, 3, &[1, 2345, 6780])).unwrap(), "12345.678");
        // -0.0042 = [42] weight -1, dscale 4
        assert_eq!(numeric(&num(1, -1, 0x4000, 4, &[42])).unwrap(), "-0.0042");
        // 1000000 = [100] weight 1 -> "100" + "0000"
        assert_eq!(numeric(&num(1, 1, 0, 0, &[100])).unwrap(), "1000000");
        assert_eq!(numeric(&num(0, 0, 0, 2, &[])).unwrap(), "0.00");
        assert_eq!(numeric(&num(0, 0, 0xC000, 0, &[])).unwrap(), "NaN");
    }

    #[test]
    fn dates_and_timestamps_from_the_2000_epoch() {
        assert_eq!(date(0), "2000-01-01");
        assert_eq!(date(-1), "1999-12-31");
        assert_eq!(date(9_190), "2025-02-28");
        assert_eq!(timestamp(0), "2000-01-01 00:00:00");
        assert_eq!(timestamp(86_400_000_000 + 3_723_500_000), "2000-01-02 01:02:03.5");
        assert_eq!(timestamp(-1), "1999-12-31 23:59:59.999999");
        assert_eq!(date(i32::MAX), "infinity");
    }

    #[test]
    fn uuid_inet_bytea_interval() {
        let u: Vec<u8> = (0u8..16).collect();
        assert_eq!(uuid(&u).unwrap(), "00010203-0405-0607-0809-0a0b0c0d0e0f");
        assert_eq!(inet(&[2, 32, 0, 4, 10, 0, 0, 1]).unwrap(), "10.0.0.1");
        assert_eq!(inet(&[2, 24, 1, 4, 10, 0, 0, 0]).unwrap(), "10.0.0.0/24");
        assert_eq!(bytea(&[0xde, 0xad]), "\\xdead");
        assert!(bytea(&[0u8; 100]).ends_with("(100 bytes)"));
        let mut iv = Vec::new();
        iv.extend(3_600_000_000i64.to_be_bytes());
        iv.extend(2i32.to_be_bytes());
        iv.extend(14i32.to_be_bytes());
        assert_eq!(interval(&iv).unwrap(), "1 year 2 mons 2 days 01:00:00");
    }

    #[test]
    fn infinite_intervals_and_hostile_arrays_do_not_panic() {
        let enc = |us: i64, d: i32, m: i32| {
            let mut v = us.to_be_bytes().to_vec();
            v.extend(d.to_be_bytes());
            v.extend(m.to_be_bytes());
            v
        };
        assert_eq!(interval(&enc(i64::MAX, i32::MAX, i32::MAX)).unwrap(), "infinity");
        assert_eq!(interval(&enc(i64::MIN, i32::MIN, i32::MIN)).unwrap(), "-infinity");
        // Mixed extremes: no overflow, some text.
        assert!(interval(&enc(i64::MIN, 0, 0)).is_some());
        // An array header claiming 2^31-1 elements with no data.
        let mut a = Vec::new();
        for x in [1i32, 0, 25, i32::MAX, 1] {
            a.extend(x.to_be_bytes());
        }
        assert_eq!(array(&Type::TEXT, &a), None);
    }

    #[test]
    fn text_arrays_quote_what_needs_quoting() {
        // ndim=1, flags=0, elem=text(25), len=3, lbound=1, then elements.
        let mut a = Vec::new();
        for x in [1i32, 0, 25, 3, 1] {
            a.extend(x.to_be_bytes());
        }
        for s in ["a", "b c", ""] {
            a.extend((s.len() as i32).to_be_bytes());
            a.extend(s.as_bytes());
        }
        assert_eq!(array(&Type::TEXT, &a).unwrap(), r#"{a,"b c",""}"#);
    }
}
