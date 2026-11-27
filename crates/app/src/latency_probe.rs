//! Keystroke → pixel latency of the real app, on the real display.
//!
//! Test harness only: compiled with the `latency-probe` feature and armed by
//! `OPSPILOT_LATENCY_PROBE=1`. Never in a normal build.
//!
//! What it measures: a key event handed to GTK for the app's own window →
//! the shell's echo visible in the window's pixels on the X server. That is
//! the whole path a real keystroke takes inside the app: GTK → WebKit → the
//! page's keydown → IPC → PTY → shell echo → parser → render → present.
//! Keyboard-to-X-server and compositor-to-scanout are outside it, and are the
//! same for every renderer.
//!
//! Nothing is sent to the desktop. Keys are synthesized as GDK events and
//! dispatched with `gtk_main_do_event` inside this process, the same as the
//! spike's harness. Pixels are read with `GetImage` on a second X connection.
//!
//! Method: open a local shell, find the block cursor, then alternate `x` and
//! BackSpace. The probe pixel sits in the top-left of the cursor's cell, above
//! the x-height, so `x` turns it from cursor amber to background and BackSpace
//! turns it back.
//!
//! Run it with a scratch `HOME` that holds a `.zshrc`: with an empty `HOME`,
//! zsh starts its new-user wizard instead of a prompt and nothing echoes.

use std::sync::{Arc, Mutex};

use opspilot_render::wgpu;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use tauri::Manager;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

const SAMPLES: usize = 160;
const WARMUP: usize = 10;
const TIMEOUT: Duration = Duration::from_secs(2);

/// Stages a key passes through, timestamped by the code that handles it.
#[derive(Clone, Copy)]
pub enum Stage {
    /// `session_write` received the bytes from the page.
    Write = 0,
    /// The render thread started drawing a new generation.
    Draw = 1,
    /// Commands submitted; with `OPSPILOT_PROBE_GPU_WAIT=1`, the GPU finished
    /// executing them.
    Gpu = 2,
    /// The frame was handed to the presentation engine.
    Present = 3,
}

/// Diagnostics only. Block the render thread until the GPU finishes each
/// frame before presenting, so GPU time is split from the X server's.
pub fn gpu_wait() -> bool {
    std::env::var("OPSPILOT_PROBE_GPU_WAIT").as_deref() == Ok("1")
}

/// Diagnostics only. Force a present mode: `immediate`, `mailbox` or `fifo`.
pub fn present_mode() -> Option<wgpu::PresentMode> {
    match std::env::var("OPSPILOT_PROBE_PRESENT").ok()?.as_str() {
        "immediate" => Some(wgpu::PresentMode::Immediate),
        "mailbox" => Some(wgpu::PresentMode::Mailbox),
        "fifo" => Some(wgpu::PresentMode::Fifo),
        _ => None,
    }
}

/// First time each stage was reached since the current key was injected.
static MARKS: Mutex<[Option<Instant>; 4]> = Mutex::new([None; 4]);
static ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record a stage for the key in flight. One relaxed load when idle.
pub fn mark(stage: Stage) {
    if !ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Ok(mut m) = MARKS.lock() {
        let slot = &mut m[stage as usize];
        if slot.is_none() {
            // Stages are only meaningful in order: a draw before the write
            // belongs to something else.
            let prior_ok = (stage as usize == 0) || m[stage as usize - 1].is_some();
            if prior_ok {
                m[stage as usize] = Some(Instant::now());
            }
        }
    }
}

#[derive(Default)]
struct Pending {
    /// When the key was handed to GTK.
    t0: Option<Instant>,
    /// What the probe pixel turns into: amber (cursor back) or not.
    want_amber: bool,
}

pub fn start(app: tauri::AppHandle) {
    if std::env::var("OPSPILOT_LATENCY_PROBE").as_deref() != Ok("1") {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = run(app) {
            eprintln!("[latency] failed: {e}");
            std::process::exit(2);
        }
    });
}

fn run(app: tauri::AppHandle) -> Result<(), String> {
    std::thread::sleep(Duration::from_secs(4));
    let win = app.get_webview_window("main").ok_or("no main window")?;
    // `OPSPILOT_PROBE_WINDOW=WxH`: measure at a given window size.
    if let Some((w, h)) = std::env::var("OPSPILOT_PROBE_WINDOW").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?))
    }) {
        win.set_size(tauri::LogicalSize::new(w, h)).map_err(|e| e.to_string())?;
    }
    // Open a local shell the way a user would.
    win.eval(
        "[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Local shell')?.click()",
    )
    .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_secs(4));
    win.eval("document.querySelector('[aria-label=Terminal]')?.focus()").map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_secs(1));

    let top = toplevel_xid(&win)?;
    let gpu = crate::gpu_term::probe_xid();
    // The GPU path draws into the child window; the canvas into the toplevel.
    let target = gpu.unwrap_or(top);
    let mode = match (gpu.is_some(), present_mode(), gpu_wait()) {
        (false, ..) => "canvas".to_string(),
        (true, m, w) => format!("gpu{}{}", m.map(|m| format!(" {m:?}")).unwrap_or_default(), if w { " +wait" } else { "" }),
    };
    let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let (px, py) = find_cursor(&conn, target)?;
    let geo = conn.get_geometry(target).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    eprintln!("[latency:{mode}] window 0x{target:x} {}x{}, probe pixel ({px},{py})", geo.width, geo.height);

    // Each GetImage makes the X server read the window back from the GPU. A
    // gap between polls keeps the probe from loading the server it measures,
    // at the cost of up to that much resolution.
    let poll_gap = Duration::from_micros(
        std::env::var("OPSPILOT_PROBE_POLL_US").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
    );
    let pending = Arc::new(Mutex::new(Pending::default()));
    let mut samples: Vec<f64> = Vec::new();
    // `OPSPILOT_PROBE_SAMPLES=n`: more samples, for looking at the tail.
    let want_samples = std::env::var("OPSPILOT_PROBE_SAMPLES").ok().and_then(|v| v.parse().ok()).unwrap_or(SAMPLES);
    // `OPSPILOT_PROBE_CSV=path`: every sample, for finding a pattern in the
    // slow ones (when, which key, which stage).
    let mut csv = std::env::var("OPSPILOT_PROBE_CSV").ok().map(|path| {
        (path, String::from("i,unix_ms,t_ms,key,gap_ms,total_ms,key_write,write_draw,draw_gpu,gpu_present,present_pixel,last_poll_ms,polls\n"))
    });
    let started = Instant::now();
    // Per sample: key → write, write → draw, draw → gpu, gpu → present,
    // present → pixel.
    let mut stages: Vec<[f64; 5]> = Vec::new();
    let mut timeouts = 0;
    let mut seed = 0x9E37_79B9u64;
    let mut type_x = true;
    while samples.len() < want_samples {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let gap = 120 + seed % 200;
        std::thread::sleep(Duration::from_millis(gap));
        let (mut polls, mut last_poll) = (0u32, Duration::ZERO);

        {
            let mut p = pending.lock().map_err(|_| "poisoned")?;
            p.t0 = None;
            p.want_amber = !type_x;
        }
        if let Ok(mut m) = MARKS.lock() {
            *m = [None; 4];
        }
        ARMED.store(true, std::sync::atomic::Ordering::Relaxed);
        let keyval = if type_x { 0x78 } else { 0xff08 }; // x, BackSpace
        let (w, pend) = (win.clone(), pending.clone());
        app.run_on_main_thread(move || {
            if let Ok(gw) = w.gtk_window() {
                if let Some(gdkwin) = gw.window() {
                    if let Ok(mut p) = pend.lock() {
                        p.t0 = Some(Instant::now());
                    }
                    inject(&gdkwin, keyval);
                }
            }
        })
        .map_err(|e| e.to_string())?;

        // Poll the pixel until it flips.
        let deadline = Instant::now() + TIMEOUT;
        let done = loop {
            if Instant::now() > deadline {
                break None;
            }
            let (t0, want) = match pending.lock() {
                Ok(p) => (p.t0, p.want_amber),
                Err(_) => return Err("poisoned".into()),
            };
            let Some(t0) = t0 else {
                std::thread::sleep(Duration::from_micros(100));
                continue;
            };
            let asked = Instant::now();
            let got = pixel(&conn, target, px, py);
            last_poll = asked.elapsed();
            polls += 1;
            let Some(c) = got else { continue };
            if is_amber(c) == want {
                break Some(t0.elapsed());
            }
            if !poll_gap.is_zero() {
                std::thread::sleep(poll_gap);
            }
        };
        ARMED.store(false, std::sync::atomic::Ordering::Relaxed);
        if let (Some(dt), Ok(m), Ok(p)) = (done, MARKS.lock(), pending.lock()) {
            if let (Some(t0), [Some(w), Some(d), Some(g), Some(pr)]) = (p.t0, *m) {
                let ms = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs_f64() * 1000.0;
                let st = [ms(t0, w), ms(w, d), ms(d, g), ms(g, pr), ms(pr, t0 + dt)];
                stages.push(st);
                if let Some((_, out)) = csv.as_mut() {
                    out.push_str(&format!(
                        "{},{:.3},{:.3},{},{gap},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{polls}\n",
                        samples.len(),
                        unix_ms(t0),
                        ms(started, t0),
                        if type_x { "x" } else { "bs" },
                        dt.as_secs_f64() * 1000.0,
                        st[0],
                        st[1],
                        st[2],
                        st[3],
                        st[4],
                        last_poll.as_secs_f64() * 1000.0,
                    ));
                }
            }
        }
        match done {
            Some(dt) => samples.push(dt.as_secs_f64() * 1000.0),
            None => {
                timeouts += 1;
                if timeouts > 5 {
                    return Err(format!("{timeouts} keys never showed up; is the terminal focused?"));
                }
            }
        }
        type_x = !type_x;
    }

    if let Some((path, out)) = csv {
        if let Err(e) = std::fs::write(&path, out) {
            eprintln!("[latency] could not write {path}: {e}");
        }
    }
    let mut v = samples[WARMUP..].to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    eprintln!(
        "[latency:{mode}] n={} p50 {:.2} ms  p90 {:.2}  p99 {:.2}  max {:.2}  mean {:.2}  min {:.2}  timeouts {timeouts}",
        v.len(),
        q(0.5),
        q(0.9),
        q(0.99),
        v[v.len() - 1],
        mean,
        v[0]
    );
    if stages.len() > WARMUP {
        let gpu = if gpu_wait() { "draw→gpu done (build, GPU executes)" } else { "draw→submit (build, encode)" };
        let names = ["key→write (GTK, key hook or WebKit+IPC)", "write→draw (PTY, shell echo, parse, wake)", gpu, "→present (present call)", "present→pixel (driver, X server)"];
        for (i, name) in names.iter().enumerate() {
            let mut v: Vec<f64> = stages[WARMUP..].iter().map(|s| s[i]).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
            eprintln!("[latency:{mode}]   {name:<44} p50 {:.2} ms  p90 {:.2}  p99 {:.2}", q(0.5), q(0.9), q(0.99));
        }
    }
    // The normal quit path, so a crash on exit shows up here too.
    app.exit(0);
    Ok(())
}

fn toplevel_xid(win: &tauri::WebviewWindow) -> Result<u32, String> {
    let gw = win.gtk_window().map_err(|e| e.to_string())?;
    let w = gw.window().ok_or("not realized")?;
    w.downcast_ref::<gdkx11::X11Window>().map(|x| x.xid() as u32).ok_or_else(|| "not X11".into())
}

/// Press and release one key in this app's window, through GTK's normal event
/// dispatch. In-process only: no other client sees it.
fn inject(window: &gtk::gdk::Window, keyval: u32) {
    use gtk::glib::translate::ToGlibPtr;
    use gtk::gdk::ffi as gdk;
    let display = window.display();
    let keycode = gtk::gdk::Keymap::for_display(&display)
        .and_then(|k| k.entries_for_keyval(keyval).first().map(|e| e.keycode()))
        .unwrap_or(0);
    for kind in [gdk::GDK_KEY_PRESS, gdk::GDK_KEY_RELEASE] {
        // SAFETY: a freshly allocated event, filled field by field. The window
        // reference is owned by the event and released by gdk_event_free.
        unsafe {
            let ev = gdk::gdk_event_new(kind);
            let key = &mut (*ev).key;
            key.window = window.to_glib_full();
            key.send_event = 0;
            key.time = gdk::GDK_CURRENT_TIME as u32;
            key.state = 0;
            key.keyval = keyval;
            key.hardware_keycode = keycode as u16;
            key.group = 0;
            key.is_modifier = 0;
            let seat = gdk::gdk_display_get_default_seat(display.to_glib_none().0);
            gdk::gdk_event_set_device(ev, gdk::gdk_seat_get_keyboard(seat));
            gtk::ffi::gtk_main_do_event(ev);
            gdk::gdk_event_free(ev);
        }
    }
}

fn pixel(conn: &impl Connection, win: u32, x: i16, y: i16) -> Option<(u8, u8, u8)> {
    let img = conn.get_image(ImageFormat::Z_PIXMAP, win, x, y, 1, 1, !0).ok()?.reply().ok()?;
    let d = &img.data;
    (d.len() >= 3).then(|| (d[2], d[1], d[0]))
}

/// `--amber-500` is #F2A93B.
fn is_amber((r, g, b): (u8, u8, u8)) -> bool {
    r > 215 && (140..=195).contains(&g) && b < 110
}

/// Top-left of the block cursor: the first amber pixel with a solid 5×10 amber
/// block below and right of it. The pane's 1 px focus ring and the 2 px tab
/// underline are amber too, but never that solid.
fn find_cursor(conn: &impl Connection, win: u32) -> Result<(i16, i16), String> {
    let geo = conn.get_geometry(win).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    let (w, h) = (geo.width as usize, geo.height as usize);
    let img = conn
        .get_image(ImageFormat::Z_PIXMAP, win, 0, 0, geo.width, geo.height, !0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let px = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        (img.data[i + 2], img.data[i + 1], img.data[i])
    };
    for y in 0..h.saturating_sub(10) {
        for x in 0..w.saturating_sub(5) {
            if is_amber(px(x, y)) && (0..10).all(|dy| (0..5).all(|dx| is_amber(px(x + dx, y + dy)))) {
                return Ok(((x + 1) as i16, (y + 2) as i16));
            }
        }
    }
    Err("no block cursor on screen".into())
}

/// Wall-clock time of an `Instant`, in ms since the epoch, to line probe
/// samples up with outside recordings (other processes' GPU time).
fn unix_ms(at: Instant) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    now - at.elapsed().as_secs_f64() * 1000.0
}

