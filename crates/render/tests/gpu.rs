//! The whole path on a real device: VT bytes → screen → quads → pixels.
//!
//! Skips (loudly) where there is no adapter. Set `OPSPILOT_RENDER_DUMP=dir` to
//! write the frames as PPM for a look by eye.

use opspilot_core_term::{Parser, Screen};
use opspilot_render::{gpu, CursorDraw, Frame, Glyphs, Fonts, Palette, Point, Rect, Renderer};

const PAD: u32 = 6;

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    glyphs: Glyphs,
    frame: Frame,
    size: (u32, u32),
}

fn rig(rows: usize, cols: usize) -> Option<Rig> {
    let Some((device, queue)) = gpu::headless() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return None;
    };
    let renderer = Renderer::new(&device, &queue, wgpu::TextureFormat::Bgra8Unorm);
    let glyphs = Glyphs::new(Fonts::new(12.5, 17));
    let m = glyphs.fonts.metrics;
    let mut frame = Frame::new(m, Palette::default(), PAD);
    frame.resize(rows, cols);
    let size = (2 * PAD + cols as u32 * m.width, 2 * PAD + rows as u32 * m.height);
    Some(Rig { device, queue, renderer, glyphs, frame, size })
}

impl Rig {
    fn draw(&mut self, screen: &Screen) -> Vec<u8> {
        let stale: Vec<usize> = self.frame.stale_rows(&self.glyphs).collect();
        for r in stale {
            let (g, s) = screen.grid().row(r);
            self.frame.set_row(&mut self.glyphs, r, g, s, screen.styles());
        }
        let bg = self.frame.palette.bg;
        let inst = self.frame.instances(&mut self.glyphs).to_vec();
        self.renderer.upload(&mut self.glyphs);
        self.renderer.read_back(self.size, bg, &inst).expect("read back")
    }

    /// `(r, g, b)` at a pixel of a BGRA frame.
    fn px(&self, img: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
        let i = ((y * self.size.0 + x) * 4) as usize;
        (img[i + 2], img[i + 1], img[i])
    }

    fn cell_ink(&self, img: &[u8], row: u32, col: u32, bg: (u8, u8, u8)) -> usize {
        let m = self.glyphs.fonts.metrics;
        let (x0, y0) = (PAD + col * m.width, PAD + row * m.height);
        (y0..y0 + m.height)
            .flat_map(|y| (x0..x0 + m.width).map(move |x| (x, y)))
            .filter(|&(x, y)| self.px(img, x, y) != bg)
            .count()
    }

    fn dump(&self, name: &str, img: &[u8]) {
        let Ok(dir) = std::env::var("OPSPILOT_RENDER_DUMP") else { return };
        let mut ppm = format!("P6 {} {} 255\n", self.size.0, self.size.1).into_bytes();
        for p in img.chunks(4) {
            ppm.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        std::fs::write(format!("{dir}/{name}.ppm"), ppm).expect("write dump");
    }
}

fn hex(c: u32) -> (u8, u8, u8) {
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

#[test]
fn text_colours_and_backgrounds_land_in_their_cells() {
    let Some(mut rig) = rig(6, 40) else { return };
    let mut screen = Screen::new(6, 40);
    Parser::new().advance(
        &mut screen,
        b"plain \x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m\r\n\
          \x1b[31mred\x1b[0m \x1b[42m green bg \x1b[0m \x1b[7mrev\x1b[0m\r\n\
          \x1b[38;2;10;200;30mtruecolor\x1b[0m \x1b[38;5;208m256\x1b[0m\r\n",
    );
    Parser::new().advance(&mut screen, "┌──┬──┐ ▀▄█░ →●λЖ\r\n└──┴──┘ 日本 ok".as_bytes());
    let img = rig.draw(&screen);
    rig.dump("text", &img);
    let p = Palette::default();
    let bg = hex(p.bg);

    // The padding is background.
    assert_eq!(rig.px(&img, 1, 1), bg);
    // `p` has ink, the space after `plain` has none.
    assert!(rig.cell_ink(&img, 0, 0, bg) > 5);
    assert_eq!(rig.cell_ink(&img, 0, 5, bg), 0);
    // Green background covers the whole cell (row 1, col 4 is the first space).
    let m = rig.glyphs.fonts.metrics;
    let (x, y) = (PAD + 4 * m.width, PAD + m.height);
    assert_eq!(rig.px(&img, x, y), hex(p.ansi[2]));
    assert_eq!(rig.px(&img, x + m.width - 1, y + m.height - 1), hex(p.ansi[2]));
    // Red text: its most covered pixel is (nearly) the palette red. Not exact:
    // hinting is vertical only, as FreeType's is under WebKit, so a stem can
    // peak a little under full coverage.
    let red = hex(p.ansi[1]);
    let dist = |c: (u8, u8, u8)| (c.0 as i32 - red.0 as i32).abs() + (c.1 as i32 - red.1 as i32).abs() + (c.2 as i32 - red.2 as i32).abs();
    let best = (0..m.height)
        .flat_map(|dy| (0..m.width).map(move |dx| (dx, dy)))
        .map(|(dx, dy)| dist(rig.px(&img, PAD + dx, PAD + m.height + dy)))
        .min()
        .unwrap_or(i32::MAX);
    assert!(best <= 16, "`r` is not drawn in palette red (closest pixel off by {best})");
    // Box drawing: `│`-free, but `┌` and `└` must meet across the row gap,
    // i.e. the down arm of `┌` reaches the bottom pixel row of its cell.
    let row3_bottom = PAD + 4 * m.height - 1;
    let ink_at_bottom = (0..m.width).any(|dx| rig.px(&img, PAD + dx, row3_bottom) != bg);
    assert!(ink_at_bottom, "┌ stops short of the cell edge");
}

#[test]
fn cursor_inverts_the_character_under_it_and_selection_tints() {
    let Some(mut rig) = rig(3, 20) else { return };
    let mut screen = Screen::new(3, 20);
    Parser::new().advance(&mut screen, b"abc");
    let p = Palette::default();
    let (g, s) = screen.grid().row(0);
    rig.frame.set_cursor(Some(CursorDraw { row: 0, col: 1, glyph: g[1], style: screen.styles().get(s[1]), focused: true }));
    rig.frame.set_selection(Some((Point { row: 1, col: 0 }, Point { row: 1, col: 3 })));
    let img = rig.draw(&screen);
    rig.dump("cursor", &img);
    let m = rig.glyphs.fonts.metrics;

    // Cursor cell: mostly cursor colour, with some background-coloured `b` ink.
    let (x0, y0) = (PAD + m.width, PAD);
    let mut amber = 0;
    let mut ink = 0;
    for y in y0..y0 + m.height {
        for x in x0..x0 + m.width {
            match rig.px(&img, x, y) {
                c if c == hex(p.cursor) => amber += 1,
                _ => ink += 1,
            }
        }
    }
    assert!(amber > (m.width * m.height) as usize / 2, "block cursor missing");
    assert!(ink > 5, "the `b` under the cursor was not drawn");

    // Selection: background blended 32% toward amber, not opaque amber.
    let c = rig.px(&img, PAD + 1, PAD + m.height + 1);
    let mix = |a: u32, b: u32| ((a as f32) * 0.32 + (b as f32) * 0.68).round() as i32;
    let (cr, cg, cb) = hex(p.cursor);
    let (br, bgc, bb) = hex(p.bg);
    let want = (mix(cr as u32, br as u32), mix(cg as u32, bgc as u32), mix(cb as u32, bb as u32));
    let close = |a: u8, b: i32| (a as i32 - b).abs() <= 2;
    assert!(close(c.0, want.0) && close(c.1, want.1) && close(c.2, want.2), "selection {c:?} want {want:?}");
}

#[test]
fn an_incremental_frame_matches_a_full_rebuild() {
    let Some(mut rig) = rig(4, 30) else { return };
    let mut screen = Screen::new(4, 30);
    Parser::new().advance(&mut screen, b"first line\r\nsecond");
    rig.draw(&screen);
    screen.grid_mut().clear_damage();
    Parser::new().advance(&mut screen, b" more\r\n\x1b[1;33mthird\x1b[0m");
    // Rebuild only what the grid says changed.
    let damaged: Vec<usize> = screen.grid().dirty_rows().collect();
    assert!(!damaged.contains(&0), "row 0 should be untouched");
    for r in damaged {
        let (g, s) = screen.grid().row(r);
        rig.frame.set_row(&mut rig.glyphs, r, g, s, screen.styles());
    }
    let incremental = rig.draw(&screen);
    rig.frame.invalidate();
    let full = rig.draw(&screen);
    assert!(incremental == full, "damage-driven frame differs from a full rebuild");
}

impl Rig {
    /// A persistent render target, as the presenter's canvas is.
    fn canvas(&self) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: self.size.0, height: self.size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Build what changed, then draw only the damage into `canvas`.
    fn draw_into(&mut self, screen: &Screen, canvas: &wgpu::Texture) -> Option<Rect> {
        let stale: Vec<usize> = self.frame.stale_rows(&self.glyphs).collect();
        for r in stale {
            let (g, s) = screen.grid().row(r);
            self.frame.set_row(&mut self.glyphs, r, g, s, screen.styles());
        }
        let damage = self.frame.take_damage(self.size);
        let full = damage == Some(Rect::full(self.size));
        let bg = self.frame.palette.bg;
        let inst = self.frame.instances(&mut self.glyphs).to_vec();
        self.renderer.upload(&mut self.glyphs);
        let view = canvas.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        if damage.is_some() {
            self.renderer.draw_damage(&mut enc, &view, self.size, bg, &inst, if full { None } else { damage });
        }
        self.queue.submit([enc.finish()]);
        damage
    }

    fn read(&self, tex: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = self.size;
        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = buf.slice(..).get_mapped_range().expect("map");
        mapped.chunks(padded as usize).take(h as usize).flat_map(|r| r[..(w * 4) as usize].to_vec()).collect()
    }
}

#[test]
fn damage_only_frames_match_a_full_redraw() {
    let Some(mut rig) = rig(8, 30) else { return };
    let canvas = rig.canvas();
    let mut screen = Screen::new(8, 30);
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"$ ls\r\n\x1b[1;34mdir\x1b[0m  file\r\n$ ");
    assert_eq!(rig.draw_into(&screen, &canvas), Some(Rect::full(rig.size)), "first frame is full");

    let cursor = |screen: &Screen| {
        let c = screen.grid().cursor;
        let (g, s) = screen.grid().row(c.row);
        Some(CursorDraw { row: c.row, col: c.col, glyph: g[c.col], style: screen.styles().get(s[c.col]), focused: true })
    };
    // Typing, backspace, a new line, a selection appearing and moving, the
    // cursor going unfocused: each step draws only its damage, and the canvas
    // must equal a fresh full render of the same state every time.
    let steps: [&[u8]; 6] = [b"e", b"cho hi", b"\x08\x08", b"\r\nhi\r\n$ ", b"", b""];
    for (i, bytes) in steps.iter().enumerate() {
        screen.grid_mut().clear_damage();
        parser.advance(&mut screen, bytes);
        for r in screen.grid().dirty_rows().collect::<Vec<_>>() {
            rig.frame.invalidate_row(r);
        }
        let mut cur = cursor(&screen);
        match i {
            4 => rig.frame.set_selection(Some((Point { row: 1, col: 0 }, Point { row: 1, col: 2 }))),
            5 => {
                rig.frame.set_selection(Some((Point { row: 0, col: 2 }, Point { row: 0, col: 3 })));
                cur = cur.map(|c| CursorDraw { focused: false, ..c });
            }
            _ => {}
        }
        rig.frame.set_cursor(cur);
        let damage = rig.draw_into(&screen, &canvas).expect("every step changes something");
        assert!(damage.h < rig.size.1, "step {i} redrew the whole surface");
        let partial = rig.read(&canvas);

        let fresh = rig.canvas();
        rig.frame.invalidate();
        rig.draw_into(&screen, &fresh);
        let full = rig.read(&fresh);
        rig.dump(&format!("damage{i}"), &partial);
        assert!(partial == full, "step {i}: damage-only frame differs from a full redraw");
    }
}
