//! Scrollback — a fixed-capacity ring of variable-length rows.
//!
//! CLAUDE.md §5.6, both rules:
//!
//!   · "Ring buffer of rows, fixed capacity (default 100k). Append is O(1),
//!     oldest overwritten in place. Never `Vec::remove(0)` — that is O(n) per
//!     line and exactly why some emulators stutter under `yes`."
//!   · "Variable-length rows: trailing blanks are not stored. A 5-char line in
//!     an 800-col terminal costs bytes, not 800 cells."

use crate::grid::BLANK;
use crate::style::{StyleId, DEFAULT_STYLE};

pub const DEFAULT_CAPACITY: usize = 100_000;

/// §5.6: "reflow scrollback lazily on scroll in 1,000-row chunks".
pub const REFLOW_CHUNK: usize = 1_000;

/// One scrolled-off line, trimmed of trailing blanks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub glyph: Vec<u32>,
    pub style: Vec<StyleId>,
    /// This row ran out of width and continues into the next one.
    ///
    /// The same flag `Grid` carries, and for the same reason: an auto-wrapped row
    /// and a row that ended in a newline are identical in the cell planes, so
    /// without it [`Scrollback::reflow_step`] would join unrelated lines.
    pub wrapped: bool,
}

impl Row {
    /// Build from a full-width grid row, dropping the trailing run of
    /// default-styled blanks.
    ///
    /// A **wrapped** row is kept at full width instead. It is full by
    /// definition — that is why it wrapped — and trimming it would silently eat
    /// trailing spaces the user typed, which reflow would then be unable to put
    /// back.
    pub fn trimmed(glyph: &[u32], style: &[StyleId], wrapped: bool) -> Row {
        let mut end = glyph.len();
        if !wrapped {
            while end > 0 && glyph[end - 1] == BLANK && style[end - 1] == DEFAULT_STYLE {
                end -= 1;
            }
        }
        Row {
            glyph: glyph[..end].to_vec(),
            style: style[..end].to_vec(),
            wrapped,
        }
    }

    pub fn len(&self) -> usize {
        self.glyph.len()
    }

    pub fn is_empty(&self) -> bool {
        self.glyph.is_empty()
    }
}

/// Work in progress for [`Scrollback::reflow_step`].
#[derive(Debug)]
struct Reflow {
    /// Target width.
    cols: usize,
    /// Index of the next source row to consume.
    next: usize,
    /// Re-wrapped rows produced so far.
    out: Vec<Row>,
    /// A logical line still being gathered across `wrapped` rows.
    carry: Option<Row>,
}

/// Fixed-capacity ring. Once full, pushing overwrites the oldest row in place —
/// no shifting, no reallocation, no allocator traffic in steady state beyond the
/// row's own contents.
#[derive(Debug)]
pub struct Scrollback {
    rows: Vec<Row>,
    capacity: usize,
    /// Index of the next slot to write.
    head: usize,
    len: usize,
    /// Total rows ever pushed. Lets a viewport hold a stable position while
    /// output scrolls underneath it.
    total: u64,
    /// The width the retained rows are wrapped to, once known. `None` before the
    /// first resize, when nothing has asked for a particular width.
    cols: Option<usize>,
    reflow: Option<Reflow>,
}

impl Scrollback {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Scrollback {
            rows: Vec::new(),
            capacity,
            head: 0,
            len: 0,
            total: 0,
            cols: None,
            reflow: None,
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Rows ever pushed, including those since overwritten.
    #[inline]
    pub fn total_pushed(&self) -> u64 {
        self.total
    }

    /// O(1). The backing `Vec` grows only until it reaches capacity; after that
    /// every push reuses a slot.
    pub fn push(&mut self, row: Row) {
        self.total += 1;
        if self.rows.len() < self.capacity {
            self.rows.push(row);
            self.head = self.rows.len() % self.capacity;
            self.len = self.rows.len();
            return;
        }
        self.rows[self.head] = row;
        self.head = (self.head + 1) % self.capacity;
        self.len = self.capacity;
    }

    /// `0` is the oldest retained row.
    pub fn get(&self, i: usize) -> Option<&Row> {
        if i >= self.len {
            return None;
        }
        let start = if self.len < self.capacity {
            0
        } else {
            self.head
        };
        Some(&self.rows[(start + i) % self.rows.len()])
    }

    /// Oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &Row> + '_ {
        (0..self.len).filter_map(move |i| self.get(i))
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.head = 0;
        self.len = 0;
        self.reflow = None;
    }

    /* ---- Reflow (§5.6) ---------------------------------------------------- */

    /// Ask for the retained history to be re-wrapped to `cols`.
    ///
    /// Cheap: it records the target and discards any half-finished reflow to an
    /// older one. That matters because a window drag emits a resize per pointer
    /// move — the work must not start over and *complete* forty times.
    ///
    /// §5.6: "reflow the visible viewport synchronously (bounded, ~50 rows), then
    /// reflow scrollback lazily on scroll in 1,000-row chunks on a background
    /// task. Resizing with 100k rows must never block for seconds." This is the
    /// lazy half; nothing happens until [`Scrollback::reflow_step`] is called.
    pub fn request_reflow(&mut self, cols: usize) {
        if cols == 0 {
            return;
        }
        if self.cols == Some(cols) && self.reflow.is_none() {
            return;
        }
        if self.len == 0 {
            // Nothing to re-wrap; adopt the width so later rows are measured
            // against it.
            self.cols = Some(cols);
            self.reflow = None;
            return;
        }
        self.reflow = Some(Reflow {
            cols,
            next: 0,
            out: Vec::new(),
            carry: None,
        });
    }

    pub fn reflow_pending(&self) -> bool {
        self.reflow.is_some()
    }

    /// Do up to [`REFLOW_CHUNK`] rows of pending re-wrapping.
    ///
    /// Returns `true` while work remains, so a caller can drive it from a scroll
    /// handler or an idle tick and stop when it returns `false`.
    ///
    /// Readers see the **old** rows until the last chunk lands: the source ring is
    /// not touched until the swap, so history is never half-consistent. The cost
    /// of that choice is transient memory — the reflowed copy exists alongside the
    /// original until the swap — which is bounded by the history actually kept.
    ///
    /// Complexity: O(cells in the chunk). Called at most `len / REFLOW_CHUNK + 1`
    /// times per resize.
    pub fn reflow_step(&mut self) -> bool {
        let Some(mut work) = self.reflow.take() else {
            return false;
        };

        let end = (work.next + REFLOW_CHUNK).min(self.len);
        for i in work.next..end {
            let Some(src) = self.get(i) else { break };
            match &mut work.carry {
                Some(c) => {
                    c.glyph.extend_from_slice(&src.glyph);
                    c.style.extend_from_slice(&src.style);
                    c.wrapped = src.wrapped;
                }
                None => work.carry = Some(src.clone()),
            }
            // A logical line ends at the first row that did not wrap.
            if !src.wrapped {
                if let Some(line) = work.carry.take() {
                    split_into(&line, work.cols, &mut work.out);
                }
            }
        }
        work.next = end;

        if work.next < self.len {
            self.reflow = Some(work);
            return true;
        }

        // Last chunk: flush a logical line still open at the newest row. It is
        // unfinished only because the screen is still writing to it.
        if let Some(line) = work.carry.take() {
            split_into(&line, work.cols, &mut work.out);
        }
        self.replace_rows(work.out);
        self.cols = Some(work.cols);
        false
    }

    /// Swap in a re-wrapped history, keeping the newest `capacity` rows.
    ///
    /// `total` is deliberately left alone: it counts rows ever *produced by the
    /// terminal*, and a viewport holds its scroll position against it. Reflow
    /// changes how history is wrapped, not how much of it happened.
    fn replace_rows(&mut self, mut rows: Vec<Row>) {
        if rows.len() > self.capacity {
            rows.drain(..rows.len() - self.capacity);
        }
        self.len = rows.len();
        self.head = if self.len < self.capacity { self.len % self.capacity } else { 0 };
        self.rows = rows;
    }
}

/// Split one logical line into rows of `cols`, marking every row but the last as
/// wrapped. A line that fits produces exactly one row; an empty line produces one
/// empty row, because a blank line in history is a blank line on screen.
fn split_into(line: &Row, cols: usize, out: &mut Vec<Row>) {
    let len = line.glyph.len();
    let chunks = len.div_ceil(cols).max(1);
    for c in 0..chunks {
        let lo = c * cols;
        let hi = ((c + 1) * cols).min(len);
        out.push(Row {
            glyph: line.glyph[lo..hi].to_vec(),
            style: line.style[lo..hi].to_vec(),
            // The final row inherits the source line's own state: if the screen
            // is still writing to it, it is still a continuation.
            wrapped: if c + 1 < chunks { true } else { line.wrapped },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str) -> Row {
        Row {
            glyph: s.bytes().map(|b| b as u32).collect(),
            style: vec![DEFAULT_STYLE; s.len()],
            wrapped: false,
        }
    }

    /// A row that continues into the next one.
    fn cont(s: &str) -> Row {
        Row { wrapped: true, ..row(s) }
    }

    fn text(r: &Row) -> String {
        r.glyph
            .iter()
            .map(|&g| char::from_u32(g).unwrap())
            .collect()
    }

    #[test]
    fn trailing_blanks_are_not_stored() {
        let glyph: Vec<u32> = "hi"
            .bytes()
            .map(|b| b as u32)
            .chain(std::iter::repeat(BLANK).take(798))
            .collect();
        let style = vec![DEFAULT_STYLE; 800];
        let r = Row::trimmed(&glyph, &style, false);
        assert_eq!(
            r.len(),
            2,
            "a 2-char line in an 800-col terminal must cost 2 cells"
        );
    }

    /// A wrapped row is full by definition, and its trailing spaces are content
    /// the user typed. Trimming them would leave reflow unable to put them back.
    #[test]
    fn a_wrapped_row_keeps_its_trailing_spaces() {
        let glyph: Vec<u32> = "hi".bytes().map(|b| b as u32).chain([BLANK; 6]).collect();
        let style = vec![DEFAULT_STYLE; 8];
        assert_eq!(Row::trimmed(&glyph, &style, true).len(), 8);
        assert_eq!(Row::trimmed(&glyph, &style, false).len(), 2);
    }

    #[test]
    fn styled_blanks_are_kept() {
        // A blank with a background colour is visible content, not padding.
        let glyph = vec![BLANK; 4];
        let mut style = vec![DEFAULT_STYLE; 4];
        style[2] = 7;
        let r = Row::trimmed(&glyph, &style, false);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn ring_overwrites_oldest_and_stays_bounded() {
        let mut sb = Scrollback::new(3);
        for c in ["a", "b", "c", "d", "e"] {
            sb.push(row(c));
        }
        assert_eq!(sb.len(), 3);
        let got: Vec<String> = sb.iter().map(text).collect();
        assert_eq!(got, vec!["c", "d", "e"]);
        assert_eq!(sb.total_pushed(), 5);
    }

    #[test]
    fn get_is_oldest_first_before_and_after_wrap() {
        let mut sb = Scrollback::new(4);
        sb.push(row("a"));
        sb.push(row("b"));
        assert_eq!(text(sb.get(0).unwrap()), "a");
        assert_eq!(text(sb.get(1).unwrap()), "b");
        assert!(sb.get(2).is_none());

        for c in ["c", "d", "e", "f"] {
            sb.push(row(c));
        }
        let got: Vec<String> = sb.iter().map(text).collect();
        assert_eq!(got, vec!["c", "d", "e", "f"]);
    }

    /* ---- Reflow (§5.6) ---------------------------------------------------- */

    /// Drive a reflow to completion the way a caller would.
    fn finish_reflow(sb: &mut Scrollback, cols: usize) {
        sb.request_reflow(cols);
        let mut guard = 0;
        while sb.reflow_step() {
            guard += 1;
            assert!(guard < 10_000, "reflow did not terminate");
        }
    }

    #[test]
    fn widening_rejoins_a_wrapped_line_in_history() {
        let mut sb = Scrollback::new(100);
        // "hello world" as it was wrapped at 6 columns.
        sb.push(cont("hello "));
        sb.push(row("world"));
        sb.push(row("$ ls"));

        finish_reflow(&mut sb, 40);

        let got: Vec<String> = sb.iter().map(text).collect();
        assert_eq!(got, vec!["hello world", "$ ls"]);
        assert!(!sb.get(0).unwrap().wrapped, "it fits now");
    }

    #[test]
    fn narrowing_splits_a_long_line_in_history() {
        let mut sb = Scrollback::new(100);
        sb.push(row("abcdefgh"));

        finish_reflow(&mut sb, 3);

        let got: Vec<String> = sb.iter().map(text).collect();
        assert_eq!(got, vec!["abc", "def", "gh"]);
        // Every row but the last continues, or a later widening would treat them
        // as three separate lines.
        assert!(sb.get(0).unwrap().wrapped);
        assert!(sb.get(1).unwrap().wrapped);
        assert!(!sb.get(2).unwrap().wrapped);
    }

    /// The property the `wrapped` flag exists for. Two lines that happen to be
    /// adjacent must not be joined.
    #[test]
    fn unwrapped_neighbours_are_never_joined() {
        let mut sb = Scrollback::new(100);
        sb.push(row("first"));
        sb.push(row("second"));

        finish_reflow(&mut sb, 200);

        let got: Vec<String> = sb.iter().map(text).collect();
        assert_eq!(got, vec!["first", "second"]);
    }

    /// Round trip: narrow, then widen back, and the history is what it was.
    ///
    /// This is the one that would fail on any of the plausible mistakes — losing
    /// the flag on split, trimming a wrapped row, or flushing the carry early.
    #[test]
    fn a_narrow_then_widen_round_trip_restores_the_lines() {
        let mut sb = Scrollback::new(100);
        let lines = ["the quick brown fox", "", "jumps over", "the lazy dog"];
        for l in lines {
            sb.push(row(l));
        }

        finish_reflow(&mut sb, 4);
        finish_reflow(&mut sb, 80);

        let got: Vec<String> = sb.iter().map(text).collect();
        assert_eq!(got, lines.to_vec());
    }

    /// §5.6's chunking, and the reason it exists: a resize must not do the work.
    #[test]
    fn reflow_is_chunked_and_does_nothing_until_stepped() {
        let mut sb = Scrollback::new(10_000);
        for i in 0..(REFLOW_CHUNK * 2 + 5) {
            sb.push(row(&format!("line {i}")));
        }
        let before = sb.len();

        sb.request_reflow(4);
        assert!(sb.reflow_pending());
        assert_eq!(sb.len(), before, "requesting a reflow must not do it");

        // Two full chunks, then the remainder.
        assert!(sb.reflow_step());
        assert!(sb.reflow_step());
        assert!(!sb.reflow_step(), "the third step covers the last 5 rows");
        assert!(!sb.reflow_pending());
        assert!(sb.len() > before, "narrowing to 4 columns splits every line");
    }

    /// A drag resize emits a resize per pointer move. Restarting is correct;
    /// completing forty reflows is not.
    #[test]
    fn a_new_target_width_abandons_the_previous_reflow() {
        let mut sb = Scrollback::new(10_000);
        for i in 0..(REFLOW_CHUNK + 10) {
            sb.push(row(&format!("line {i}")));
        }
        sb.request_reflow(4);
        assert!(sb.reflow_step(), "one chunk done, more to go");

        // The user is still dragging.
        sb.request_reflow(9);
        finish_reflow(&mut sb, 9);

        // Every row is wrapped to 9, not to 4 — the abandoned work left nothing
        // behind.
        assert!(sb.iter().all(|r| r.len() <= 9));
        assert!(sb.iter().any(|r| r.len() > 4));
    }

    /// Reflow must not resurrect history the ring already dropped.
    #[test]
    fn reflow_respects_capacity() {
        let mut sb = Scrollback::new(8);
        for i in 0..40 {
            sb.push(row(&format!("line {i}")));
        }
        assert_eq!(sb.len(), 8);

        finish_reflow(&mut sb, 3); // splits each row into three
        assert_eq!(sb.len(), 8, "the ring stays bounded");
        // And the newest content is what survived, as it does on push.
        let last = text(sb.get(7).unwrap());
        assert!("line 39".contains(&last), "kept {last} instead of the newest");
    }

    #[test]
    fn reflowing_empty_history_is_a_no_op() {
        let mut sb = Scrollback::new(8);
        sb.request_reflow(20);
        assert!(!sb.reflow_pending());
        assert!(!sb.reflow_step());
        assert_eq!(sb.len(), 0);
    }

    #[test]
    fn capacity_is_never_exceeded_under_flood() {
        let mut sb = Scrollback::new(64);
        for i in 0..10_000 {
            sb.push(row(&format!("line {i}")));
        }
        assert_eq!(sb.len(), 64);
        assert_eq!(text(sb.get(63).unwrap()), "line 9999");
    }
}
