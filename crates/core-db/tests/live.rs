//! Against real servers. Ignored by default; run with the test containers up:
//!
//! ```text
//! docker run -d --name opspilot-test-pg -e POSTGRES_PASSWORD=opspilot -e POSTGRES_DB=testdb \
//!   -p 127.0.0.1:55432:5432 postgres:16-alpine
//! docker run -d --name opspilot-test-mysql -e MYSQL_ROOT_PASSWORD=opspilot -e MYSQL_DATABASE=testdb \
//!   -p 127.0.0.1:53306:3306 mysql:8.4
//! cargo test -p opspilot-core-db --test live -- --ignored --test-threads=1
//! ```

use std::time::{Duration, Instant};

use opspilot_core_db::model::{Query, Window, CHUNK};
use opspilot_core_db::session::{Engine, Session, Target};

fn target(engine: Engine) -> Target {
    let (port, user) = match engine {
        Engine::Postgres => (55432, "postgres"),
        Engine::Mysql => (53306, "root"),
        Engine::Sqlite => unreachable!(),
    };
    Target {
        engine,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        password: Some("opspilot".into()),
        database: "testdb".into(),
        path: None,
    }
}

async fn done(q: &Query) -> Window {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let w = q.window(0, 50);
        if w.done || Instant::now() > deadline {
            return w;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn exec(s: &Session, sql: &str) -> Window {
    let w = done(&s.run(sql.into())).await;
    assert!(w.error.is_none(), "{sql}: {:?}", w.error);
    w
}

/// Wait until `loaded` stops changing: the producer is parked on demand.
async fn settle(q: &Query) -> usize {
    let mut last = usize::MAX;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        // Read the counter without raising demand past the first chunk.
        let w = q.window(0, 0);
        if w.loaded == last {
            return last;
        }
        last = w.loaded;
    }
    last
}

async fn common(engine: Engine, setup: &[&str]) {
    let s = Session::open(target(engine)).await.expect("connect");
    eprintln!("{engine:?}: {}", s.label);
    for sql in setup {
        exec(&s, sql).await;
    }

    // 1. Streaming: one chunk until the UI asks for more.
    let q = s.run("select id, label, amount from big order by id".into());
    let parked = settle(&q).await;
    assert!(
        (CHUNK..1234).contains(&parked),
        "{engine:?} should park after a chunk, loaded {parked}"
    );
    let w = q.window(1200, 34); // scroll to the end
    let _ = w;
    let w = done(&q).await;
    assert_eq!(w.loaded, 1234, "{engine:?}");

    // 2. Sort descending by a numeric column, in Rust.
    q.set_view(Some((2, true)), String::new());
    let w = q.window(0, 3);
    assert_eq!(w.rows[0][0].as_deref(), Some("1234"), "{engine:?}: {:?}", w.rows[0]);

    // 3. Filter.
    q.set_view(None, "label-12".into());
    let w = q.window(0, 100);
    // label-12, label-120..129, label-1200..1234
    assert_eq!(w.view_rows, 1 + 10 + 35, "{engine:?}");

    // 4. DML reports the affected count.
    let w = exec(&s, "update big set amount = amount + 1 where id <= 10").await;
    assert_eq!(w.affected, Some(10), "{engine:?}");

    // 5. Server errors come back with the server's message.
    let w = done(&s.run("select no_such_column from big".into())).await;
    assert!(w.error.as_deref().unwrap_or("").contains("no_such_column"), "{engine:?}: {:?}", w.error);

    // 6. A running query is cancelled by the next one; the session survives.
    let slow = match engine {
        Engine::Postgres => "select pg_sleep(30)",
        _ => "select sleep(30)",
    };
    let q = s.run(slow.into());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t0 = Instant::now();
    let w = exec(&s, "select 1 as one").await;
    assert!(t0.elapsed() < Duration::from_secs(10), "{engine:?}: cancel took {:?}", t0.elapsed());
    assert_eq!(w.rows[0][0].as_deref(), Some("1"));
    assert!(q.window(0, 0).done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the postgres test container"]
async fn postgres_live() {
    common(
        Engine::Postgres,
        &[
            "drop table if exists big",
            "create table big(id int primary key, label text, amount numeric(12,2))",
            "insert into big select g, 'label-' || g, g * 1.25 from generate_series(1, 1234) g",
        ],
    )
    .await;

    // Types decoded from the binary protocol.
    let s = Session::open(target(Engine::Postgres)).await.unwrap();
    let w = exec(
        &s,
        "select 12345.678::numeric, '2025-02-28'::date, '2025-02-28 13:14:15.5'::timestamp, \
         'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid, '{\"k\": 1}'::jsonb, '10.0.0.0/24'::cidr, \
         array['a','b c']::text[], interval '1 day 02:00:00', true, null::int",
    )
    .await;
    let r: Vec<_> = w.rows[0].iter().map(|c| c.clone().unwrap_or_else(|| "NULL".into())).collect();
    assert_eq!(
        r,
        [
            "12345.678",
            "2025-02-28",
            "2025-02-28 13:14:15.5",
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
            "{\"k\": 1}",
            "10.0.0.0/24",
            "{a,\"b c\"}",
            "1 day 02:00:00",
            "true",
            "NULL"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the mysql test container"]
async fn mysql_live() {
    common(
        Engine::Mysql,
        &[
            "drop table if exists big",
            // Session variables persist: the session is one connection.
            "set session cte_max_recursion_depth = 5000",
            "create table big(id int primary key, label varchar(32), amount decimal(12,2))",
            "insert into big with recursive g(n) as (select 1 union all select n + 1 from g where n < 1234) \
             select n, concat('label-', n), n * 1.25 from g",
        ],
    )
    .await;
}

/// Timings, printed rather than asserted (the machine is not quiet; see
/// COMPLETED.md Pending 1.3). Needs the `events` table (200k rows).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the postgres test container"]
async fn postgres_bulk_load_and_sort_timing() {
    let s = Session::open(target(Engine::Postgres)).await.unwrap();
    // Self-contained: a fresh container has no data.
    exec(&s, "drop table if exists events").await;
    exec(
        &s,
        "create table events as select g as id, 'event-' || g as name, now() - (g || ' seconds')::interval as at, \
         (random()*1000)::numeric(8,2) as amount from generate_series(1,200000) g",
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(60);
    let t0 = Instant::now();
    let q = s.run("select * from events".into());
    // First chunk visible. Every wait has a deadline: a query that fails
    // must fail the test, not hang it.
    loop {
        let w = q.window(0, 50);
        assert!(w.error.is_none(), "{:?}", w.error);
        if w.loaded > 0 {
            break;
        }
        assert!(Instant::now() < deadline, "first chunk never arrived");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let first = t0.elapsed();
    // Ask for everything (what a sort does), and time the load.
    let t1 = Instant::now();
    q.set_view(Some((3, true)), String::new());
    let w = loop {
        let w = q.window(0, 50);
        if !w.sorting {
            break w;
        }
        assert!(Instant::now() < deadline, "sort never finished");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let load_and_sort = t1.elapsed();
    assert_eq!(w.view_rows, 200_000);
    // Sort only: the data is loaded; switch the sort column.
    let t2 = Instant::now();
    q.set_view(Some((0, true)), String::new());
    let w = q.window(0, 50);
    let sort_only = t2.elapsed();
    assert_eq!(w.rows[0][0].as_deref(), Some("200000"));
    let t3 = Instant::now();
    q.set_view(None, "event-1999".into());
    let w = q.window(0, 50);
    let filter = t3.elapsed();
    eprintln!(
        "first chunk {:?} · load 200k + sort {:?} · sort only (int) {:?} · filter {:?} ({} rows) · reported {:?} ms",
        first, load_and_sort, sort_only, filter, w.view_rows, w.elapsed_ms
    );
}

/// `BEGIN; DELETE …;` then a SELECT to check: the SELECT's cursor must not
/// commit (or roll back) the user's transaction. Afterwards, ROLLBACK must
/// still bring the rows back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the postgres test container"]
async fn postgres_select_inside_a_user_transaction_leaves_it_alone() {
    let s = Session::open(target(Engine::Postgres)).await.unwrap();
    exec(&s, "drop table if exists keep").await;
    exec(&s, "create table keep(id int)").await;
    exec(&s, "insert into keep select generate_series(1, 10)").await;
    exec(&s, "begin").await;
    exec(&s, "delete from keep").await;
    let w = exec(&s, "select count(*) from keep").await;
    assert_eq!(w.rows[0][0].as_deref(), Some("0"), "the delete is visible inside the tx");
    // A failing SELECT inside the tx must not ROLLBACK the user's transaction
    // either — only the savepoint.
    let w = done(&s.run("select no_such_col from keep".into())).await;
    assert!(w.error.is_some());
    exec(&s, "rollback").await;
    let w = exec(&s, "select count(*) from keep").await;
    assert_eq!(w.rows[0][0].as_deref(), Some("10"), "the user's ROLLBACK must undo the delete");
}

/// Cancelling a cursor that is parked (nothing running on the server) must not
/// send a server-side cancel, which would arrive late and kill the NEXT query.
async fn parked_cancel_spares_the_next_query(engine: Engine, big: &str) {
    let s = Session::open(target(engine)).await.unwrap();
    for _ in 0..5 {
        let q = s.run(big.into());
        // Wait until it is parked after the first chunk.
        for _ in 0..200 {
            if q.is_parked() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(q.is_parked(), "{engine:?}: never parked");
        // Run the next query immediately: it supersedes the parked one.
        let w = exec(&s, "select 42").await;
        assert_eq!(w.rows[0][0].as_deref(), Some("42"), "{engine:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
        // And one more after any late cancel would have landed.
        let w = exec(&s, "select 43").await;
        assert_eq!(w.rows[0][0].as_deref(), Some("43"), "{engine:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the postgres test container"]
async fn postgres_parked_cancel_spares_the_next_query() {
    let s = Session::open(target(Engine::Postgres)).await.unwrap();
    exec(&s, "drop table if exists big").await;
    exec(&s, "create table big(id int)").await;
    exec(&s, "insert into big select generate_series(1, 5000)").await;
    parked_cancel_spares_the_next_query(Engine::Postgres, "select * from big").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the mysql test container"]
async fn mysql_parked_cancel_spares_the_next_query() {
    let s = Session::open(target(Engine::Mysql)).await.unwrap();
    exec(&s, "drop table if exists big").await;
    exec(&s, "set session cte_max_recursion_depth = 10000").await;
    exec(&s, "create table big(id int)").await;
    exec(
        &s,
        "insert into big with recursive g(n) as (select 1 union all select n + 1 from g where n < 5000) select n from g",
    )
    .await;
    parked_cancel_spares_the_next_query(Engine::Mysql, "select * from big").await;
}
