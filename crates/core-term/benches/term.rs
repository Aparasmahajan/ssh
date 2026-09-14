//! Criterion benchmarks for the terminal engine.
//!
//! CLAUDE.md §16: "Measure before optimizing. Add a Criterion benchmark, record
//! the number, then change the code. A PR claiming a speedup without a bench
//! delta is not reviewable."
//!
//! The §4 row these serve is **"`cat` a 200 MB file — sustained 60 fps, zero
//! dropped bytes"**. That budget is about the whole pipeline; what is measured
//! here is the parse+grid half, in bytes/second. To hold 60fps while parsing, the
//! engine must keep up with the PTY, and the useful question is "how many MB/s
//! can one core absorb".
//!
//! Run:  cargo bench -p opspilot-core-term

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use opspilot_core_term::parser::ascii_run_len;
use opspilot_core_term::{Parser, Perform, Screen};

/// Discards everything. Isolates parser cost from grid cost, so a regression can
/// be attributed to one or the other.
#[derive(Default)]
struct Sink {
    chars: u64,
}

impl Perform for Sink {
    fn print(&mut self, _c: char) {
        self.chars += 1;
    }
    fn print_ascii(&mut self, bytes: &[u8]) {
        self.chars += bytes.len() as u64;
    }
    fn execute(&mut self, _byte: u8) {}
    fn csi_dispatch(&mut self, _p: &opspilot_core_term::Params, _i: &[u8], _ig: bool, _a: u8) {}
    fn esc_dispatch(&mut self, _i: &[u8], _ig: bool, _b: u8) {}
    fn osc_dispatch(&mut self, _p: &[&[u8]], _bell: bool) {}
}

/// Plain ASCII lines — the §5.2 claim is that real output is >95% this.
fn corpus_plain(bytes: usize) -> Vec<u8> {
    let line = b"the quick brown fox jumps over the lazy dog 0123456789\r\n";
    line.iter().copied().cycle().take(bytes).collect()
}

/// Heavily coloured output: `ls --color`, build logs, test runners. Every line
/// carries SGR sequences, so the DFA runs far more often.
fn corpus_sgr(bytes: usize) -> Vec<u8> {
    let line = b"\x1b[1;32mPASS\x1b[0m \x1b[2msrc/module/thing.rs\x1b[0m 12ms\r\n";
    line.iter().copied().cycle().take(bytes).collect()
}

/// Non-ASCII, so every character goes through the incremental UTF-8 decoder
/// rather than the bulk path. This is the pessimistic case.
fn corpus_utf8(bytes: usize) -> Vec<u8> {
    let line = "héllo wörld — ✓ αβγδ 日本語テキスト\r\n".as_bytes();
    line.iter().copied().cycle().take(bytes).collect()
}

/// A `tree`/`systemd` style corpus: box drawing, which is 3-byte UTF-8.
fn corpus_boxdraw(bytes: usize) -> Vec<u8> {
    let line = "├── opspilot-core-term ─────────── ok │\r\n".as_bytes();
    line.iter().copied().cycle().take(bytes).collect()
}

const SIZE: usize = 4 * 1024 * 1024;

fn bench_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_only");
    group.throughput(Throughput::Bytes(SIZE as u64));

    for (name, data) in [
        ("plain_ascii", corpus_plain(SIZE)),
        ("sgr_heavy", corpus_sgr(SIZE)),
        ("utf8", corpus_utf8(SIZE)),
        ("box_drawing", corpus_boxdraw(SIZE)),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut parser = Parser::new();
                let mut sink = Sink::default();
                parser.advance(&mut sink, black_box(&data));
                black_box(sink.chars)
            })
        });
    }
    group.finish();
}

fn bench_full_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_and_grid");
    group.throughput(Throughput::Bytes(SIZE as u64));

    for (name, data) in [
        ("plain_ascii", corpus_plain(SIZE)),
        ("sgr_heavy", corpus_sgr(SIZE)),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                // 200x50 is the §5.1 reference viewport.
                let mut screen = Screen::new(50, 200);
                let mut parser = Parser::new();
                parser.advance(&mut screen, black_box(&data));
                black_box(screen.grid().cols())
            })
        });
    }
    group.finish();
}

fn bench_ascii_scan(c: &mut Criterion) {
    // The SWAR scan on its own. If the bulk path ever stops paying, it shows
    // here first.
    let data = corpus_plain(SIZE);
    let mut group = c.benchmark_group("ascii_run_len");
    group.throughput(Throughput::Bytes(SIZE as u64));
    group.bench_function("swar", |b| {
        b.iter(|| {
            let mut i = 0;
            let mut runs = 0u64;
            while i < data.len() {
                let n = ascii_run_len(black_box(&data[i..]));
                if n == 0 {
                    i += 1;
                } else {
                    i += n;
                    runs += 1;
                }
            }
            black_box(runs)
        })
    });
    group.finish();
}

fn bench_damage(c: &mut Criterion) {
    // §5.3 claims a clean screen costs one comparison per 64 rows. A 100k-row
    // scan that finds nothing should be effectively free.
    use opspilot_core_term::Grid;
    let mut grid = Grid::new(100_000, 200);
    grid.clear_damage();
    c.bench_function("damage_scan_clean_100k_rows", |b| {
        b.iter(|| black_box(grid.dirty_rows().count()))
    });

    let mut sparse = Grid::new(100_000, 200);
    sparse.clear_damage();
    for r in (0..100_000).step_by(5_000) {
        sparse.mark_dirty(r);
    }
    c.bench_function("damage_scan_20_of_100k_rows", |b| {
        b.iter(|| black_box(sparse.dirty_rows().count()))
    });
}

criterion_group!(
    benches,
    bench_parser,
    bench_full_pipeline,
    bench_ascii_scan,
    bench_damage
);
criterion_main!(benches);
