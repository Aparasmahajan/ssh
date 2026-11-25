//! The result store every engine writes into, and the view the UI reads.
//!
//! CLAUDE.md §11, as built:
//!
//! - **Cursor streaming.** Engines fetch [`CHUNK`] rows (500, §11's default),
//!   then *wait* until the UI's demand passes what is loaded. A 10 M-row
//!   `SELECT` costs 500 rows until someone scrolls; nothing is materialized
//!   speculatively.
//! - **Arrow columnar.** Each chunk is one typed Arrow array per column — one
//!   contiguous buffer, not a heap object per cell.
//! - **Sort and filter in Rust**, over those arrays: `sort_to_indices` on a
//!   primitive array is a radix-style sort, not a comparison sort over boxed
//!   rows. The UI only ever receives the formatted *window* it is painting.
//!
//! DataFusion is deliberately not used: it would add tens of MB to the binary
//! for single-column sort and substring filter, which arrow's kernels already
//! do. See docs/DECISIONS.md D-28.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arrow_array::builder::{BooleanBuilder, Float64Builder, Int64Builder, StringBuilder};
use arrow_array::{Array, ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray};
use arrow_schema::SortOptions;
use serde::Serialize;
use tokio::sync::Notify;

/// Rows fetched per round trip and per Arrow chunk.
pub const CHUNK: usize = 500;

/// The largest single fetch, used when the whole result is wanted.
pub const BULK: usize = 20_000;

/// Adds the time since `.1` to the counter when dropped — however the wait
/// ends (demand, cancel, or the future being dropped).
struct WaitClock<'a>(&'a AtomicU64, Instant);
impl Drop for WaitClock<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(self.1.elapsed().as_millis() as u64, Ordering::AcqRel);
    }
}

/// A hard ceiling on rows held for one result. Sorting or filtering loads the
/// whole result, and an accidental `SELECT *` on a billion-row table must stop
/// somewhere rather than take the machine down.
pub const MAX_ROWS: usize = 2_000_000;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Int,
    Float,
    Bool,
    Text,
    /// Exact decimals (`numeric`, `DECIMAL`) and SQLite's dynamically typed
    /// numeric columns: kept as text so no precision is lost on display, but
    /// sorted by numeric value.
    Numeric,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ColumnMeta {
    pub name: String,
    /// The engine's type name, for the header tooltip: "int8", "varchar".
    pub type_name: String,
    pub kind: Kind,
}

/// One value on its way into a column.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

enum Builder {
    Int(Int64Builder),
    Float(Float64Builder),
    Bool(BooleanBuilder),
    Text(StringBuilder),
}

impl Builder {
    fn new(kind: Kind) -> Builder {
        match kind {
            Kind::Int => Builder::Int(Int64Builder::with_capacity(CHUNK)),
            Kind::Float => Builder::Float(Float64Builder::with_capacity(CHUNK)),
            Kind::Bool => Builder::Bool(BooleanBuilder::with_capacity(CHUNK)),
            Kind::Text | Kind::Numeric => Builder::Text(StringBuilder::with_capacity(CHUNK, CHUNK * 16)),
        }
    }

    /// Append, coercing where an engine hands a value of a neighbouring type
    /// (a MySQL `BIGINT` arrives as text in the text protocol). A value that
    /// genuinely does not fit its column is kept as text rather than lost:
    /// a Text column takes anything, and a numeric column that cannot parse it
    /// records NULL — which the engines avoid by choosing `Text` for any column
    /// whose type they do not fully understand.
    /// Returns the approximate bytes the value occupies, for the byte budget.
    fn push(&mut self, cell: Cell) -> usize {
        let text_len = match &cell {
            Cell::Text(t) => t.len(),
            _ => 0,
        };
        match (self, cell) {
            (Builder::Int(b), Cell::Int(v)) => b.append_value(v),
            (Builder::Int(b), Cell::Text(s)) => b.append_option(s.trim().parse().ok()),
            (Builder::Int(b), Cell::Bool(v)) => b.append_value(v as i64),
            (Builder::Float(b), Cell::Float(v)) => b.append_value(v),
            (Builder::Float(b), Cell::Int(v)) => b.append_value(v as f64),
            (Builder::Float(b), Cell::Text(s)) => b.append_option(s.trim().parse().ok()),
            (Builder::Bool(b), Cell::Bool(v)) => b.append_value(v),
            (Builder::Bool(b), Cell::Int(v)) => b.append_value(v != 0),
            (Builder::Text(b), Cell::Text(s)) => b.append_value(s),
            (Builder::Text(b), Cell::Int(v)) => b.append_value(v.to_string()),
            (Builder::Text(b), Cell::Float(v)) => b.append_value(v.to_string()),
            (Builder::Text(b), Cell::Bool(v)) => b.append_value(if v { "true" } else { "false" }),
            (Builder::Int(b), _) => b.append_null(),
            (Builder::Float(b), _) => b.append_null(),
            (Builder::Bool(b), _) => b.append_null(),
            (Builder::Text(b), _) => b.append_null(),
        }
        // Fixed-width values cost their slot; text costs its bytes plus an offset.
        8 + text_len
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Builder::Int(b) => Arc::new(b.finish()),
            Builder::Float(b) => Arc::new(b.finish()),
            Builder::Bool(b) => Arc::new(b.finish()),
            Builder::Text(b) => Arc::new(b.finish()),
        }
    }
}

/* ---- Shared state between the engine task and the UI ----------------------- */

/// Bytes held for one result. Rows alone are not a bound: 2 M rows of a 1 MB
/// `jsonb` column is 2 TB, and a sort raises demand to the row cap.
pub const MAX_BYTES: usize = 512 * 1024 * 1024;

/// A chunk is flushed early past this many bytes. Arrow string arrays use i32
/// offsets, so one array over 2 GiB panics in `append_value` — and the release
/// profile is `panic = "abort"`, so that would take the whole app down.
const CHUNK_BYTES: usize = 64 * 1024 * 1024;

/// Longest cell text sent to the UI. A 50 MB value serialised over IPC every
/// poll would freeze the window; nobody reads past this in a grid cell anyway.
pub const CELL_DISPLAY_MAX: usize = 16 * 1024;

/// How long a cursor may sit parked waiting for the UI to scroll before it is
/// closed. While parked, the server holds a transaction (Postgres: a snapshot
/// that blocks VACUUM and a lock that blocks `ALTER TABLE`) or an unfinished
/// statement (MySQL: a metadata lock). Someone who walked away from the first
/// page must not be able to stall a migration indefinitely.
pub const PARK_LIMIT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Data {
    columns: Vec<ColumnMeta>,
    /// `chunks[i][c]` is column `c` of chunk `i`.
    chunks: Vec<Vec<ArrayRef>>,
    /// Row index where each chunk starts, for O(log chunks) row lookup.
    starts: Vec<usize>,
    rows: usize,
    done: bool,
    truncated: bool,
    error: Option<String>,
    /// Rows affected by a statement that returns none (UPDATE, DDL).
    affected: Option<u64>,
    message: Option<String>,
    elapsed_ms: Option<u64>,
    /// The ordered (and filtered) row indices, or `None` for "all rows, in
    /// order". Rebuilt when `view_stale`, outside the lock.
    view: Option<Vec<u32>>,
    view_stale: bool,
    /// Bumped by every change that invalidates a view being built, so a build
    /// that raced one is discarded rather than installed.
    view_version: u64,
    view_error: Option<String>,
    sort: Option<(usize, bool)>,
    filter: String,
}

pub struct Shared {
    data: Mutex<Data>,
    generation: AtomicU64,
    /// Rows the UI wants loaded. Engines fetch until `rows >= demand`.
    demand: AtomicU64,
    wake: Notify,
    cancelled: AtomicBool,
    /// Mirrors `Data::done` without the lock — `Session::cancel` reads it
    /// from the IPC thread, which must never wait behind a sort.
    done: AtomicBool,
    /// The producer is parked in `wait_for_demand`: nothing is executing on
    /// the server, so a server-side cancel would hit the NEXT query instead.
    parked: AtomicBool,
    started: Instant,
    /// Milliseconds spent parked waiting for the UI to scroll. Subtracted from
    /// the reported time: a cursor idling while someone reads the first page
    /// is not the database being slow.
    waited_ms: AtomicU64,
}

/// A running or finished query. Cheap to clone; all clones see one result.
#[derive(Clone)]
pub struct Query(Arc<Shared>);

impl Default for Query {
    fn default() -> Self {
        Query::new()
    }
}

/// Why `wait_for_demand` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The UI wants more rows.
    More,
    Cancelled,
    /// Parked longer than [`PARK_LIMIT`]: close the cursor.
    Idle,
}

impl Query {
    pub fn new() -> Query {
        Query(Arc::new(Shared {
            data: Mutex::new(Data::default()),
            generation: AtomicU64::new(1),
            demand: AtomicU64::new(CHUNK as u64),
            wake: Notify::new(),
            cancelled: AtomicBool::new(false),
            done: AtomicBool::new(false),
            parked: AtomicBool::new(false),
            started: Instant::now(),
            waited_ms: AtomicU64::new(0),
        }))
    }

    pub fn sink(&self) -> Sink {
        Sink {
            q: self.clone(),
            builders: Vec::new(),
            pending: 0,
            published: 0,
            bytes: 0,
            chunk_bytes: 0,
        }
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.wake.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn is_done(&self) -> bool {
        self.0.done.load(Ordering::Acquire)
    }

    pub fn is_parked(&self) -> bool {
        self.0.parked.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.0.generation.fetch_add(1, Ordering::Release);
    }

    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// Ask for at least `rows` loaded. Monotonic: scrolling back up never
    /// un-asks.
    pub fn want(&self, rows: u64) {
        let rows = rows.min(MAX_ROWS as u64);
        if self.0.demand.fetch_max(rows, Ordering::AcqRel) < rows {
            self.0.wake.notify_waiters();
        }
    }

    /// Set sort and filter. Either needs every row, so this raises demand to
    /// the cap; the view is rebuilt once loading finishes. Cheap: the rebuild
    /// happens in the next `window`, off this thread's lock.
    pub fn set_view(&self, sort: Option<(usize, bool)>, filter: String) {
        let need_all = sort.is_some() || !filter.is_empty();
        if let Ok(mut d) = self.0.data.lock() {
            if d.sort != sort || d.filter != filter {
                d.sort = sort;
                d.filter = filter;
                d.view_stale = true;
                d.view_version += 1;
            }
        }
        if need_all {
            self.want(MAX_ROWS as u64);
        }
        self.bump();
    }

    /// Rebuild the view if it is stale, WITHOUT holding the data lock during
    /// the sort/filter: the inputs (Arc'd chunks) are snapshotted under the
    /// lock, the work runs outside it, and the result is installed only if
    /// nothing changed meanwhile. A sort of 2 M rows therefore never blocks
    /// the producer, a `set_view`, or a cancel.
    fn refresh_view(&self) {
        let snap = {
            let Ok(d) = self.0.data.lock() else { return };
            let filtered = d.sort.is_some() || !d.filter.is_empty();
            if !d.view_stale || (filtered && !d.done) {
                return;
            }
            ViewInput {
                columns: d.columns.clone(),
                chunks: d.chunks.clone(),
                starts: d.starts.clone(),
                rows: d.rows,
                sort: d.sort,
                filter: d.filter.clone(),
                version: d.view_version,
            }
        };
        let built = build_view(&snap);
        if let Ok(mut d) = self.0.data.lock() {
            if d.view_version == snap.version && d.rows == snap.rows {
                match built {
                    Ok(v) => {
                        d.view = v;
                        d.view_error = None;
                    }
                    Err(e) => {
                        d.view = None;
                        d.view_error = Some(e);
                    }
                }
                d.view_stale = false;
            }
        }
    }

    /// The rows `[offset, offset + limit)` of the current view, formatted for
    /// display, plus the counters the status bar needs. Also raises demand to
    /// one chunk past the window — §11's "prefetch the next chunk past 70%
    /// scroll", expressed as a distance rather than a percentage.
    pub fn window(&self, offset: usize, limit: usize) -> Window {
        self.want((offset.saturating_add(limit).saturating_add(CHUNK)) as u64);
        self.refresh_view();
        let d = match self.0.data.lock() {
            Ok(d) => d,
            Err(_) => return Window::error("result poisoned"),
        };
        let filtered = d.sort.is_some() || !d.filter.is_empty();
        // Sort/filter over a partly loaded result would reorder under the
        // user as chunks arrive; wait for the load to finish instead.
        let ready = (!filtered || d.done) && !d.view_stale;
        let total = if !ready && filtered {
            0
        } else {
            d.view.as_ref().map_or(d.rows, |v| v.len())
        };
        let end = offset.saturating_add(limit).min(total);
        let mut rows = Vec::with_capacity(end.saturating_sub(offset));
        for i in offset..end {
            let r = d.view.as_ref().map_or(i, |v| v[i] as usize);
            rows.push(format_row(&d, r, CELL_DISPLAY_MAX));
        }
        Window {
            generation: self.generation(),
            columns: d.columns.clone(),
            offset,
            rows,
            view_rows: total,
            loaded: d.rows,
            done: d.done,
            truncated: d.truncated,
            sorting: filtered && !ready,
            error: d.error.clone().or_else(|| d.view_error.clone()),
            affected: d.affected,
            message: d.message.clone(),
            elapsed_ms: d.elapsed_ms,
        }
    }

    /// Every loaded row of the current view, untruncated, for export.
    pub fn for_each_row(&self, mut f: impl FnMut(&[ColumnMeta], Vec<Option<String>>)) {
        self.refresh_view();
        let Ok(d) = self.0.data.lock() else { return };
        let n = d.view.as_ref().map_or(d.rows, |v| v.len());
        for i in 0..n {
            let r = d.view.as_ref().map_or(i, |v| v[i] as usize);
            f(&d.columns, format_row(&d, r, usize::MAX));
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Window {
    pub generation: u64,
    pub columns: Vec<ColumnMeta>,
    pub offset: usize,
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows in the current view (after filter). What the scrollbar spans.
    pub view_rows: usize,
    pub loaded: usize,
    pub done: bool,
    pub truncated: bool,
    /// A sort or filter is waiting for the result to finish loading.
    pub sorting: bool,
    pub error: Option<String>,
    pub affected: Option<u64>,
    pub message: Option<String>,
    pub elapsed_ms: Option<u64>,
}

impl Window {
    fn error(e: &str) -> Window {
        Window {
            generation: 0,
            columns: Vec::new(),
            offset: 0,
            rows: Vec::new(),
            view_rows: 0,
            loaded: 0,
            done: true,
            truncated: false,
            sorting: false,
            error: Some(e.to_string()),
            affected: None,
            message: None,
            elapsed_ms: None,
        }
    }
}

/* ---- The producer side ------------------------------------------------------- */

/// What an engine writes through. One per query.
pub struct Sink {
    q: Query,
    builders: Vec<Builder>,
    pending: usize,
    /// Rows already flushed, counted here so `loaded()` and `full()` — called
    /// per row by the engines — never take the data lock.
    published: usize,
    bytes: usize,
    chunk_bytes: usize,
}

impl Sink {
    pub fn query(&self) -> &Query {
        &self.q
    }

    pub fn set_columns(&mut self, columns: Vec<ColumnMeta>) {
        self.builders = columns.iter().map(|c| Builder::new(c.kind)).collect();
        if let Ok(mut d) = self.q.0.data.lock() {
            d.columns = columns;
        }
        self.q.bump();
    }

    /// Append one row. Missing trailing cells are NULL; extra cells are
    /// ignored — neither happens with a well-behaved driver.
    pub fn push_row(&mut self, cells: impl IntoIterator<Item = Cell>) {
        let mut it = cells.into_iter();
        let mut row_bytes = 0usize;
        for b in &mut self.builders {
            row_bytes += b.push(it.next().unwrap_or(Cell::Null));
        }
        self.pending += 1;
        self.bytes = self.bytes.saturating_add(row_bytes);
        self.chunk_bytes = self.chunk_bytes.saturating_add(row_bytes);
        if self.pending >= CHUNK || self.chunk_bytes >= CHUNK_BYTES {
            self.flush();
        }
    }

    /// Publish buffered rows as a chunk.
    pub fn flush(&mut self) {
        if self.pending == 0 {
            return;
        }
        let arrays: Vec<ArrayRef> = self.builders.iter_mut().map(Builder::finish).collect();
        let full = self.full_after(self.pending);
        if let Ok(mut d) = self.q.0.data.lock() {
            let start = d.rows;
            d.starts.push(start);
            d.chunks.push(arrays);
            d.rows += self.pending;
            d.view_stale = true;
            d.view_version += 1;
            if full {
                d.truncated = true;
            }
        }
        self.published += self.pending;
        self.pending = 0;
        self.chunk_bytes = 0;
        self.q.bump();
    }

    pub fn loaded(&self) -> usize {
        self.published + self.pending
    }

    fn full_after(&self, extra: usize) -> bool {
        self.published + extra >= MAX_ROWS || self.bytes >= MAX_BYTES
    }

    /// True when the row or byte cap is reached; the engine should stop.
    pub fn full(&self) -> bool {
        self.full_after(self.pending)
    }

    /// Wait until the UI wants more rows than are loaded, the query is
    /// cancelled, or the cursor has been parked for [`PARK_LIMIT`].
    pub async fn wait_for_demand(&mut self) -> Wait {
        self.flush();
        let _clock = WaitClock(&self.q.0.waited_ms, Instant::now());
        let _parked = Parked::new(&self.q.0.parked);
        let deadline = tokio::time::Instant::now() + PARK_LIMIT;
        loop {
            // Register for wakeups BEFORE checking: a cancel or a scroll that
            // lands between the check and the await would otherwise be lost,
            // and the producer would wait forever holding the connection.
            let notified = self.q.0.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.q.is_cancelled() {
                return Wait::Cancelled;
            }
            if self.q.0.demand.load(Ordering::Acquire) as usize > self.loaded() {
                return Wait::More;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Wait::Idle;
            }
        }
    }

    /// The same, for engines on a plain thread (SQLite). Polls at 20 ms, which
    /// is invisible next to a scroll and costs nothing while idle.
    pub fn wait_for_demand_blocking(&mut self) -> Wait {
        self.flush();
        let _clock = WaitClock(&self.q.0.waited_ms, Instant::now());
        let _parked = Parked::new(&self.q.0.parked);
        let deadline = Instant::now() + PARK_LIMIT;
        loop {
            if self.q.is_cancelled() {
                return Wait::Cancelled;
            }
            if self.q.0.demand.load(Ordering::Acquire) as usize > self.loaded() {
                return Wait::More;
            }
            if Instant::now() >= deadline {
                return Wait::Idle;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Rows to ask the server for next: one chunk while the UI is paging, up
    /// to [`BULK`] at a time when it wants everything (a sort or filter), so a
    /// bulk load over a 40 ms link costs tens of round trips, not hundreds.
    pub fn batch_size(&self) -> usize {
        let want = self.q.0.demand.load(Ordering::Acquire) as usize;
        want.saturating_sub(self.loaded()).clamp(CHUNK, BULK)
    }

    fn active_ms(&self) -> u64 {
        let total = self.q.0.started.elapsed().as_millis() as u64;
        total.saturating_sub(self.q.0.waited_ms.load(Ordering::Acquire))
    }

    /// Mark the result as stopped early (cap, idle, cancel) without failing.
    pub fn note_truncated(&mut self, why: &str) {
        self.flush();
        if let Ok(mut d) = self.q.0.data.lock() {
            d.truncated = true;
            d.message = Some(why.to_string());
        }
        self.q.bump();
    }

    pub fn finish(mut self, affected: Option<u64>, message: Option<String>) {
        self.flush();
        let elapsed = self.active_ms();
        if let Ok(mut d) = self.q.0.data.lock() {
            d.done = true;
            d.affected = affected;
            if message.is_some() {
                d.message = message;
            }
            d.elapsed_ms = Some(elapsed);
            d.view_stale = true;
            d.view_version += 1;
        }
        self.q.0.done.store(true, Ordering::Release);
        self.q.bump();
    }

    pub fn fail(mut self, error: String) {
        self.flush();
        let elapsed = self.active_ms();
        if let Ok(mut d) = self.q.0.data.lock() {
            d.done = true;
            d.error = Some(error);
            d.elapsed_ms = Some(elapsed);
        }
        self.q.0.done.store(true, Ordering::Release);
        self.q.bump();
    }
}

/// A sink dropped without `finish`/`fail` — a panicking engine, or a task
/// aborted on close — must still mark the query done, or the UI would show
/// "running…" forever and `Session::cancel` would keep trying to stop it.
impl Drop for Sink {
    fn drop(&mut self) {
        if !self.q.is_done() {
            if let Ok(mut d) = self.q.0.data.lock() {
                if !d.done {
                    d.done = true;
                    if d.error.is_none() {
                        d.error = Some("query stopped".into());
                    }
                }
            }
            self.q.0.done.store(true, Ordering::Release);
            self.q.bump();
        }
    }
}

/// Sets `parked` for the duration of a wait, however it ends.
struct Parked<'a>(&'a AtomicBool);
impl<'a> Parked<'a> {
    fn new(f: &'a AtomicBool) -> Self {
        f.store(true, Ordering::Release);
        Parked(f)
    }
}
impl Drop for Parked<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/* ---- View: sort + filter ------------------------------------------------------ */

/// What a view build needs, snapshotted under the lock. Cloning is cheap: the
/// chunks are `Arc`s.
struct ViewInput {
    columns: Vec<ColumnMeta>,
    chunks: Vec<Vec<ArrayRef>>,
    starts: Vec<usize>,
    rows: usize,
    sort: Option<(usize, bool)>,
    filter: String,
    version: u64,
}

fn locate(starts: &[usize], row: usize) -> Option<(usize, usize)> {
    // O(log chunks).
    let i = match starts.binary_search(&row) {
        Ok(i) => i,
        Err(i) => i.checked_sub(1)?,
    };
    Some((i, row - starts[i]))
}

/// Display text of one cell, cut at `max` bytes (on a char boundary).
fn cell_text(a: &ArrayRef, i: usize, max: usize) -> Option<String> {
    if a.is_null(i) {
        return None;
    }
    let any = a.as_any();
    if let Some(x) = any.downcast_ref::<Int64Array>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<Float64Array>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<BooleanArray>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<StringArray>() {
        let v = x.value(i);
        if v.len() <= max {
            return Some(v.to_string());
        }
        let mut cut = max;
        while !v.is_char_boundary(cut) {
            cut -= 1;
        }
        return Some(format!("{}… ({} bytes)", &v[..cut], v.len()));
    }
    None
}

fn format_row(d: &Data, row: usize, max: usize) -> Vec<Option<String>> {
    match locate(&d.starts, row) {
        Some((c, i)) => d.chunks[c].iter().map(|a| cell_text(a, i, max)).collect(),
        None => vec![None; d.columns.len()],
    }
}

/// One column as a single array, concatenated across chunks.
fn column(v: &ViewInput, c: usize) -> Result<Option<ArrayRef>, String> {
    let parts: Vec<&dyn Array> = v.chunks.iter().filter_map(|ch| ch.get(c)).map(|a| a.as_ref()).collect();
    if parts.is_empty() {
        return Ok(None);
    }
    arrow_select::concat::concat(&parts).map(Some).map_err(|e| e.to_string())
}

/// The ordered, filtered row indices; `None` means identity (no sort, no
/// filter). O(n log n) for the sort, O(n × columns) for the filter.
fn build_view(v: &ViewInput) -> Result<Option<Vec<u32>>, String> {
    if v.sort.is_none() && v.filter.is_empty() {
        return Ok(None);
    }
    let mut order: Vec<u32> = match v.sort {
        Some((c, desc)) if c < v.columns.len() => sort_indices(v, c, desc)?,
        _ => (0..v.rows as u32).collect(),
    };
    if !v.filter.is_empty() {
        let mask = filter_mask(v, &v.filter);
        order.retain(|&r| mask.get(r as usize).copied().unwrap_or(false));
    }
    Ok(Some(order))
}

fn sort_indices(v: &ViewInput, c: usize, desc: bool) -> Result<Vec<u32>, String> {
    let Some(arr) = column(v, c).map_err(|e| format!("sort failed: {e}"))? else {
        return Ok((0..v.rows as u32).collect());
    };
    // Numeric-as-text sorts by value: parse once into a primitive array, so
    // the sort itself is still over f64s, not strings.
    let arr: ArrayRef = if v.columns[c].kind == Kind::Numeric {
        match arr.as_any().downcast_ref::<StringArray>() {
            Some(s) => Arc::new(
                s.iter()
                    .map(|x| x.and_then(|t| t.trim().parse::<f64>().ok()))
                    .collect::<Float64Array>(),
            ),
            None => arr,
        }
    } else {
        arr
    };
    let opts = SortOptions {
        descending: desc,
        // NULLs last either way: they are the least interesting rows.
        nulls_first: false,
    };
    // A failed sort is reported, never shown as if it had worked.
    arrow_ord::sort::sort_to_indices(&arr, Some(opts), None)
        .map(|idx| idx.values().to_vec())
        .map_err(|e| format!("sort failed: {e}"))
}

/// Case-insensitive substring over every column's display text. Strings are
/// matched in place; numbers are formatted into one reused buffer, so the scan
/// allocates nothing per cell.
fn filter_mask(v: &ViewInput, needle: &str) -> Vec<bool> {
    use std::fmt::Write;
    let needle = needle.to_lowercase();
    let mut mask = vec![false; v.rows];
    let mut buf = String::new();
    for (ci, chunk) in v.chunks.iter().enumerate() {
        let base = v.starts[ci];
        for a in chunk {
            let any = a.as_any();
            for i in 0..a.len() {
                let Some(slot) = mask.get_mut(base + i) else { break };
                if *slot || a.is_null(i) {
                    continue;
                }
                let hit = if let Some(x) = any.downcast_ref::<StringArray>() {
                    contains_ci(x.value(i), &needle)
                } else {
                    buf.clear();
                    if let Some(x) = any.downcast_ref::<Int64Array>() {
                        let _ = write!(buf, "{}", x.value(i));
                    } else if let Some(x) = any.downcast_ref::<Float64Array>() {
                        let _ = write!(buf, "{}", x.value(i));
                    } else if let Some(x) = any.downcast_ref::<BooleanArray>() {
                        let _ = write!(buf, "{}", x.value(i));
                    }
                    contains_ci(&buf, &needle)
                };
                if hit {
                    *slot = true;
                }
            }
        }
    }
    mask
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    if hay.is_ascii() && needle_lower.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle_lower.as_bytes());
        return n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    hay.to_lowercase().contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<ColumnMeta> {
        vec![
            ColumnMeta { name: "id".into(), type_name: "int8".into(), kind: Kind::Int },
            ColumnMeta { name: "name".into(), type_name: "text".into(), kind: Kind::Text },
            ColumnMeta { name: "price".into(), type_name: "numeric".into(), kind: Kind::Numeric },
        ]
    }

    fn filled(n: usize) -> Query {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(cols());
        for i in 0..n {
            s.push_row([
                Cell::Int(i as i64),
                Cell::Text(format!("row{i}")),
                // 9.5 sorts below 10.25 numerically, above it as text.
                Cell::Text(if i % 2 == 0 { "10.25".into() } else { "9.5".into() }),
            ]);
        }
        s.finish(None, None);
        q
    }

    #[test]
    fn rows_land_in_chunks_and_windows_read_across_them() {
        let q = filled(CHUNK * 2 + 7);
        let w = q.window(CHUNK - 2, 4);
        assert_eq!(w.loaded, CHUNK * 2 + 7);
        assert!(w.done);
        let ids: Vec<_> = w.rows.iter().map(|r| r[0].clone().unwrap()).collect();
        assert_eq!(ids, ["498", "499", "500", "501"]);
    }

    #[test]
    fn sort_descending_and_numeric_text_sorts_by_value() {
        let q = filled(10);
        q.set_view(Some((0, true)), String::new());
        let w = q.window(0, 3);
        assert_eq!(w.rows[0][0].as_deref(), Some("9"));

        q.set_view(Some((2, false)), String::new());
        let w = q.window(0, 10);
        assert_eq!(w.rows[0][2].as_deref(), Some("9.5"), "numeric, not lexicographic");
        assert_eq!(w.rows[9][2].as_deref(), Some("10.25"));
    }

    #[test]
    fn filter_is_case_insensitive_across_columns() {
        let q = filled(30);
        q.set_view(None, "ROW2".into());
        let w = q.window(0, 100);
        // row2, row20..row29
        assert_eq!(w.view_rows, 11);
        assert_eq!(w.loaded, 30);
    }

    #[test]
    fn nulls_sort_last_and_render_as_none() {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(vec![ColumnMeta { name: "v".into(), type_name: "int".into(), kind: Kind::Int }]);
        s.push_row([Cell::Null]);
        s.push_row([Cell::Int(2)]);
        s.push_row([Cell::Int(1)]);
        s.finish(None, None);
        q.set_view(Some((0, false)), String::new());
        let w = q.window(0, 3);
        let v: Vec<_> = w.rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(v, [Some("1".into()), Some("2".into()), None]);
    }

    #[test]
    fn a_sort_waits_for_the_load_instead_of_reordering_under_the_user() {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(cols());
        s.push_row([Cell::Int(1), Cell::Text("a".into()), Cell::Null]);
        s.flush();
        q.set_view(Some((0, true)), String::new());
        let w = q.window(0, 10);
        assert!(w.sorting);
        assert_eq!(w.view_rows, 0);
        s.finish(None, None);
        let w = q.window(0, 10);
        assert!(!w.sorting);
        assert_eq!(w.view_rows, 1);
    }

    #[tokio::test]
    async fn the_producer_waits_for_demand_and_wakes_when_the_ui_scrolls() {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(cols());
        for i in 0..CHUNK {
            s.push_row([Cell::Int(i as i64), Cell::Null, Cell::Null]);
        }
        let ui = q.clone();
        let waiter = tokio::spawn(async move {
            let more = s.wait_for_demand().await;
            (more == Wait::More, s.loaded())
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "must pause at one chunk until asked");
        ui.window(300, 300); // scrolled near the end of what is loaded
        let (more, loaded) = waiter.await.unwrap();
        assert!(more);
        assert_eq!(loaded, CHUNK);
    }

    #[tokio::test]
    async fn cancel_releases_a_waiting_producer() {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(cols());
        for i in 0..CHUNK {
            s.push_row([Cell::Int(i as i64), Cell::Null, Cell::Null]);
        }
        let h = tokio::spawn(async move { s.wait_for_demand().await });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        q.cancel();
        assert_eq!(h.await.unwrap(), Wait::Cancelled);
    }

    #[tokio::test]
    async fn a_cancel_racing_the_wait_is_never_lost() {
        // Cancel BEFORE the producer starts waiting: it must still return,
        // not sleep until the park limit.
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(cols());
        for i in 0..CHUNK {
            s.push_row([Cell::Int(i as i64), Cell::Null, Cell::Null]);
        }
        q.cancel();
        let r = tokio::time::timeout(std::time::Duration::from_secs(1), s.wait_for_demand()).await;
        assert_eq!(r.unwrap(), Wait::Cancelled);
    }

    #[tokio::test(start_paused = true)]
    async fn a_parked_cursor_is_closed_after_the_idle_limit() {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(cols());
        for i in 0..CHUNK {
            s.push_row([Cell::Int(i as i64), Cell::Null, Cell::Null]);
        }
        let h = tokio::spawn(async move { s.wait_for_demand().await });
        tokio::task::yield_now().await;
        assert!(q.is_parked());
        tokio::time::advance(PARK_LIMIT + std::time::Duration::from_secs(1)).await;
        assert_eq!(h.await.unwrap(), Wait::Idle);
        assert!(!q.is_parked(), "parked flag cleared however the wait ends");
    }

    #[test]
    fn the_byte_budget_stops_a_result_of_huge_cells() {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(vec![ColumnMeta { name: "blob".into(), type_name: "text".into(), kind: Kind::Text }]);
        let big = "x".repeat(8 * 1024 * 1024);
        let mut pushed = 0;
        while !s.full() {
            s.push_row([Cell::Text(big.clone())]);
            pushed += 1;
            assert!(pushed < 1000, "byte budget never tripped");
        }
        // ~512 MB / 8 MB, and chunks were flushed early (64 MB each), well
        // before the 2 GiB Arrow offset limit.
        assert!((60..=70).contains(&pushed), "{pushed}");
        s.finish(None, None);
        let w = q.window(0, 1);
        // Display is truncated; the full value stays in the store.
        let cell = w.rows[0][0].clone().unwrap();
        assert!(cell.len() < CELL_DISPLAY_MAX + 64, "{}", cell.len());
        assert!(cell.ends_with("(8388608 bytes)"));
    }

    #[test]
    fn a_dropped_sink_marks_the_query_done() {
        let q = Query::new();
        let s = q.sink();
        drop(s); // an engine that panicked or was aborted
        assert!(q.is_done());
        assert!(q.window(0, 1).error.is_some());
    }

    #[test]
    fn mismatched_values_coerce_or_become_null_never_panic() {
        let q = Query::new();
        let mut s = q.sink();
        s.set_columns(vec![
            ColumnMeta { name: "i".into(), type_name: "".into(), kind: Kind::Int },
            ColumnMeta { name: "t".into(), type_name: "".into(), kind: Kind::Text },
        ]);
        s.push_row([Cell::Text("42".into()), Cell::Float(1.5)]);
        s.push_row([Cell::Text("x".into()), Cell::Bool(true)]);
        s.finish(None, None);
        let w = q.window(0, 2);
        assert_eq!(w.rows[0], [Some("42".into()), Some("1.5".into())]);
        assert_eq!(w.rows[1], [None, Some("true".into())]);
    }
}
