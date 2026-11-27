//! CPU cost of a frame: grid rows → instances. The GPU side is one buffer
//! write and one draw call; this is the part that scales with the screen.

use criterion::{criterion_group, criterion_main, Criterion};
use opspilot_core_term::{Parser, Screen};
use opspilot_render::{Fonts, Frame, Glyphs, Palette};

const ROWS: usize = 50;
const COLS: usize = 200;

/// A dense, coloured screen: every cell holds a glyph, styles change often.
fn busy_screen() -> Screen {
    let mut s = Screen::new(ROWS, COLS);
    let mut p = Parser::new();
    for r in 0..ROWS {
        let mut line = String::new();
        for c in 0..COLS / 10 {
            line.push_str(&format!("\x1b[3{}m{:<10}", (r + c) % 8, format!("word{}", r * c)));
        }
        p.advance(&mut s, line.as_bytes());
        if r + 1 < ROWS {
            p.advance(&mut s, b"\r\n");
        }
    }
    s
}

fn bench(c: &mut Criterion) {
    let screen = busy_screen();
    let mut glyphs = Glyphs::new(Fonts::new(12.5, 17));
    let mut frame = Frame::new(glyphs.fonts.metrics, Palette::default(), 6);
    frame.resize(ROWS, COLS);

    c.bench_function("full 200x50 rebuild", |b| {
        b.iter(|| {
            frame.invalidate();
            let stale: Vec<usize> = frame.stale_rows(&glyphs).collect();
            for r in stale {
                let (g, st) = screen.grid().row(r);
                frame.set_row(&mut glyphs, r, g, st, screen.styles());
            }
            frame.instances(&mut glyphs).len()
        })
    });

    c.bench_function("one row changed (typing)", |b| {
        b.iter(|| {
            let (g, st) = screen.grid().row(ROWS - 1);
            frame.set_row(&mut glyphs, ROWS - 1, g, st, screen.styles());
            frame.instances(&mut glyphs).len()
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
