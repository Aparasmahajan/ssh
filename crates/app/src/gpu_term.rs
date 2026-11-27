//! The GPU terminal: a native child window per terminal pane, drawn by wgpu.
//!
//! This is the hybrid the spike measured (docs/spikes/wgpu-compose.md on
//! `spike/wgpu-compose`): an X11 child window over the pane, presenting
//! directly (3.86 ms p50 keystroke → pixel, against ~40 ms for the canvas).
//!
//! Who does what:
//!
//! - **The webview** still owns layout, input, selection and the overlays. It
//!   reports each pane's rect, focus, selection and whether an overlay covers
//!   it (`gpu_pane_set`). While covered, the child hides and the webview's
//!   canvas, which it keeps current, shows instead: overlays stay on top with
//!   no compositing tricks.
//! - **The GTK main thread** owns the child windows (`CHILDREN`). The child
//!   has an empty input region, so every click and wheel event falls through
//!   to the webview underneath.
//! - **One render thread** (§13.2: not tokio) owns the device, the atlas and
//!   every surface. It draws a pane when its session's generation moves:
//!   once per frame for output (§5.4, coalesced), and, for the 50 ms after a
//!   keystroke, the moment the session's reader says output landed, so an
//!   echo is drawn as soon as it exists rather than at the next frame.
//!
//! **Keys skip the webview too.** While a pane drawn here has focus, a GTK
//! key-press hook on the toplevel encodes keys natively (`crate::keys`) and
//! writes them to the PTY before WebKit sees them: 1.5 ms of every keystroke
//! was WebKit, the page and IPC (CLAUDE.md §18.24). App chords, dead keys and
//! anything the encoder does not know still go to the page.
//!
//! X11 only. On Wayland, or with no usable adapter, `gpu_pane_set` fails and
//! the webview keeps its canvas and its own key handling.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use opspilot_render::wgpu;
use opspilot_render::wgpu::rwh::{HasDisplayHandle, RawWindowHandle, XlibWindowHandle};
use opspilot_render::{CursorDraw, Fonts, Frame, Glyphs, Need, Palette, Point, Presenter, Rect as PixelRect, Renderer};
use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::session::{registry, SessionId};

/// Inset of the text from the pane edge, CSS px. The canvas path's PADDING.
const PAD_CSS: f64 = 6.0;
/// Output frames are coalesced to this (§5.4). 60 Hz; §14.6 wants the real
/// refresh rate, which is not read yet.
const FRAME: Duration = Duration::from_micros(16_667);
/// After a keystroke, output is drawn the moment it arrives for this long.
/// Measured on the real display: polling for the echo every 2 ms instead cost
/// a steady 2.1 ms of the keystroke budget.
const BOOST_FOR: Duration = Duration::from_millis(50);

/// End of the current post-keystroke window, µs since `epoch()`.
static BOOST_UNTIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// An `Output` message is queued and not yet handled.
static OUTPUT_PENDING: AtomicBool = AtomicBool::new(false);

fn now_us() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

pub const UNAVAILABLE: &str = "gpu terminal unavailable";

#[derive(Deserialize, Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Everything the webview knows about a pane, sent whenever any of it changes.
#[derive(Deserialize, Clone, Debug)]
pub struct PaneState {
    pub pane: String,
    pub session: SessionId,
    /// CSS px, relative to the webview.
    pub rect: Rect,
    pub dpr: f64,
    pub font_px: f64,
    pub line_px: f64,
    /// False while an overlay covers the pane.
    pub visible: bool,
    pub focused: bool,
    /// Viewport cells, inclusive, in any order.
    pub selection: Option<[[usize; 2]; 2]>,
    /// Where keystrokes go: this pane's session, or every pane in the tab
    /// while broadcasting. Empty means just `session`.
    #[serde(default)]
    pub targets: Vec<SessionId>,
}

#[derive(Serialize, Clone, Copy, Debug)]
pub struct PaneInfo {
    /// Cell size in CSS px. The webview must lay the grid out with these, not
    /// its own canvas measurement, or the two disagree on the column count.
    pub cell_w: f64,
    pub cell_h: f64,
    /// The child window is up and drawing. Until then the canvas paints.
    pub live: bool,
}

/* ---- Shared state ----------------------------------------------------------- */

static FAILED: AtomicBool = AtomicBool::new(false);
/// Why the GPU path is off, for diagnostics. A silent fallback to the canvas
/// would otherwise look exactly like the GPU path working slowly.
static WHY: Mutex<Option<String>> = Mutex::new(None);

fn fail(reason: impl Into<String>) {
    let reason = reason.into();
    if !FAILED.swap(true, Ordering::AcqRel) {
        eprintln!("gpu terminal off, using the canvas: {reason}");
    }
    if let Ok(mut g) = WHY.lock() {
        g.get_or_insert(reason);
    }
}

/// Why the GPU terminal is unavailable, if it is.
pub fn unavailable_reason() -> Option<String> {
    WHY.lock().ok().and_then(|g| g.clone())
}
static SENDER: OnceLock<SyncSender<Msg>> = OnceLock::new();
static INSTANCE: OnceLock<wgpu::Instance> = OnceLock::new();
/// The most recent child window's X id, for the latency probe.
static LAST_XID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The X window of the most recently attached pane, once it is drawing.
pub fn probe_xid() -> Option<u32> {
    let any_live = LIVE.lock().map(|g| g.as_ref().is_some_and(|s| !s.is_empty())).unwrap_or(false);
    let x = LAST_XID.load(Ordering::Acquire);
    (any_live && x != 0).then_some(x as u32)
}

/// Panes whose child window is shown and has been drawn into.
static LIVE: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The focused GPU pane, which native key handling types into.
struct Focus {
    pane: String,
    targets: Vec<SessionId>,
    /// Whose modes (DECCKM) decide the encoding.
    session: SessionId,
}

static FOCUS: Mutex<Option<Focus>> = Mutex::new(None);

fn clear_focus(pane: Option<&str>) {
    if let Ok(mut f) = FOCUS.lock() {
        if pane.is_none() || f.as_ref().is_some_and(|x| Some(x.pane.as_str()) == pane) {
            *f = None;
        }
    }
}

struct Child {
    win: gtk::gdk::Window,
    /// The render thread has configured the surface.
    ready: bool,
    want_visible: bool,
}

thread_local! {
    /// Main thread only: GDK objects are not `Send`.
    static CHILDREN: RefCell<HashMap<String, Child>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FontKey {
    px: f32,
    line: u32,
}

#[derive(Clone, Debug)]
struct Want {
    session: SessionId,
    size: (u32, u32),
    pad: u32,
    font: FontKey,
    visible: bool,
    focused: bool,
    selection: Option<(Point, Point)>,
}

enum Msg {
    Attach { pane: String, surface: wgpu::Surface<'static>, want: Want },
    Update { pane: String, want: Want },
    Detach { pane: String },
    /// The pane's window was just mapped. The server has nothing of ours in
    /// it (a present that raced the map was dropped), so the next present
    /// must cover the whole window.
    Exposed { pane: String },
    /// A keystroke was written: draw now (a prediction may already be there).
    Wake,
    /// A session's reader parsed output during the post-keystroke window.
    Output,
}

fn set_live(pane: &str, live: bool) {
    if let Ok(mut g) = LIVE.lock() {
        let set = g.get_or_insert_with(HashSet::new);
        if live {
            set.insert(pane.to_string());
        } else {
            set.remove(pane);
        }
    }
}

fn is_live(pane: &str) -> bool {
    LIVE.lock().map(|g| g.as_ref().is_some_and(|s| s.contains(pane))).unwrap_or(false)
}

/* ---- Main thread: commands --------------------------------------------------- */

/// Create, move, show or hide a pane's child window. Runs on the GTK main
/// thread (a sync command), and does only window-system calls there: the
/// drawing happens on the render thread.
pub fn set(window: &tauri::WebviewWindow, st: PaneState) -> Result<PaneInfo, String> {
    if FAILED.load(Ordering::Acquire) {
        return Err(UNAVAILABLE.into());
    }
    // An escape hatch for users, and how the canvas is measured on the same
    // build. Read once.
    static DISABLED: OnceLock<bool> = OnceLock::new();
    if *DISABLED.get_or_init(|| std::env::var("OPSPILOT_GPU_TERM").as_deref() == Ok("0")) {
        fail("disabled by OPSPILOT_GPU_TERM=0");
        return Err(UNAVAILABLE.into());
    }
    let dpr = if st.dpr > 0.0 { st.dpr } else { 1.0 };
    let font = FontKey { px: (st.font_px * dpr) as f32, line: (st.line_px * dpr).round().max(1.0) as u32 };
    let m = metrics(font);
    let size = (((st.rect.w * dpr).round() as u32).max(1), ((st.rect.h * dpr).round() as u32).max(1));
    let want = Want {
        session: st.session,
        size,
        pad: (PAD_CSS * dpr).round() as u32,
        font,
        visible: st.visible,
        focused: st.focused,
        selection: st.selection.map(|[a, b]| (Point { row: a[0], col: a[1] }, Point { row: b[0], col: b[1] })),
    };
    if let Ok(mut f) = FOCUS.lock() {
        // Focused AND uncovered. While a palette or dialog is over the pane,
        // every key goes through the page, which knows where DOM focus really
        // is; typing into the shell behind a dialog is the failure this rules
        // out, whatever the timing of focus updates.
        if st.focused && st.visible {
            let targets = if st.targets.is_empty() { vec![st.session] } else { st.targets.clone() };
            *f = Some(Focus { pane: st.pane.clone(), targets, session: st.session });
        } else if f.as_ref().is_some_and(|x| x.pane == st.pane) {
            *f = None;
        }
    }
    let info = PaneInfo { cell_w: m.width as f64 / dpr, cell_h: m.height as f64 / dpr, live: is_live(&st.pane) };

    let exists = CHILDREN.with(|c| c.borrow().contains_key(&st.pane));
    let (ox, oy) = webview_origin(window);
    let geom = (ox + st.rect.x.round() as i32, oy + st.rect.y.round() as i32, st.rect.w.round().max(1.0) as i32, st.rect.h.round().max(1.0) as i32);
    if exists {
        let mut mapped = false;
        CHILDREN.with(|c| {
            if let Some(ch) = c.borrow_mut().get_mut(&st.pane) {
                ch.win.move_resize(geom.0, geom.1, geom.2, geom.3);
                ch.want_visible = st.visible;
                if ch.ready {
                    if st.visible {
                        mapped = show_synced(&ch.win);
                    } else {
                        ch.win.hide();
                    }
                }
            }
        });
        let pane = st.pane.clone();
        send(Msg::Update { pane: st.pane, want })?;
        if mapped {
            send(Msg::Exposed { pane })?;
        }
        return Ok(info);
    }

    let (win, surface) = create_child(window, geom)?;
    CHILDREN.with(|c| c.borrow_mut().insert(st.pane.clone(), Child { win, ready: false, want_visible: st.visible }));
    if let Err(e) = send(Msg::Attach { pane: st.pane.clone(), surface, want }) {
        destroy_child(&st.pane);
        return Err(e);
    }
    Ok(info)
}

/// The pane is gone (unmounted, or switched back to the canvas).
pub fn remove(pane: String) {
    set_live(&pane, false);
    clear_focus(Some(&pane));
    CHILDREN.with(|c| {
        if let Some(ch) = c.borrow_mut().get_mut(&pane) {
            ch.win.hide();
        }
    });
    // The surface must be dropped before its X window is destroyed, so the
    // render thread drops it and then asks the main thread to destroy the
    // window. Without a render thread there is no surface either.
    if send(Msg::Detach { pane: pane.clone() }).is_err() {
        destroy_child(&pane);
    }
}

/// Called after keyboard input reaches a session.
pub fn wake() {
    BOOST_UNTIL.store(now_us() + BOOST_FOR.as_micros() as u64, Ordering::Release);
    if let Some(tx) = SENDER.get() {
        // Full means the render thread already has work queued, and will see
        // the new generation anyway.
        let _ = tx.try_send(Msg::Wake);
    }
}

/// Called by session readers after parsing output. Outside the window after a
/// keystroke it returns at once; inside it, at most one message is in flight,
/// so a flood that starts right after Enter cannot fill the channel.
pub fn output() {
    if now_us() >= BOOST_UNTIL.load(Ordering::Acquire) || OUTPUT_PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(tx) = SENDER.get() {
        if tx.try_send(Msg::Output).is_err() {
            OUTPUT_PENDING.store(false, Ordering::Release);
        }
    }
}

fn send(msg: Msg) -> Result<(), String> {
    let tx = SENDER.get().ok_or(UNAVAILABLE)?;
    match tx.try_send(msg) {
        Ok(()) => Ok(()),
        // Bounded (§13.1 rule 7). The render thread drains it every few ms;
        // full means it is wedged, and blocking the UI thread on it would
        // freeze the window too.
        Err(TrySendError::Full(_)) => Err("gpu terminal busy".into()),
        Err(TrySendError::Disconnected(_)) => Err(UNAVAILABLE.into()),
    }
}

/// Cell metrics for a font, cached: the webview asks on every layout change.
fn metrics(font: FontKey) -> opspilot_render::CellMetrics {
    static CACHE: Mutex<Option<(FontKey, opspilot_render::CellMetrics)>> = Mutex::new(None);
    if let Ok(mut g) = CACHE.lock() {
        if let Some((k, m)) = *g {
            if k == font {
                return m;
            }
        }
        let m = Fonts::new(font.px, font.line).metrics;
        *g = Some((font, m));
        return m;
    }
    Fonts::new(font.px, font.line).metrics
}

/// Where the webview's (0, 0) is in the toplevel GDK window. Zero with
/// server-side decorations; with client-side ones it is the decoration margin.
fn webview_origin(window: &tauri::WebviewWindow) -> (i32, i32) {
    fn find(w: &gtk::Widget) -> Option<gtk::Widget> {
        if w.type_().name().starts_with("WebKitWebView") {
            return Some(w.clone());
        }
        let c = w.downcast_ref::<gtk::Container>()?;
        c.children().iter().find_map(find)
    }
    let Ok(gw) = window.gtk_window() else { return (0, 0) };
    let top: gtk::Widget = gw.clone().upcast();
    find(&top).and_then(|wv| wv.translate_coordinates(&top, 0, 0)).unwrap_or((0, 0))
}

fn create_child(window: &tauri::WebviewWindow, geom: (i32, i32, i32, i32)) -> Result<(gtk::gdk::Window, wgpu::Surface<'static>), String> {
    let gw = window.gtk_window().map_err(|e| e.to_string())?;
    let parent = gw.window().ok_or("window not realized")?;
    if parent.display().downcast_ref::<gdkx11::X11Display>().is_none() {
        // Wayland: GDK would make the child a subsurface, which is untested.
        fail("not an X11 display");
        return Err(UNAVAILABLE.into());
    }
    let attr = gtk::gdk::WindowAttr {
        window_type: gtk::gdk::WindowType::Child,
        wclass: gtk::gdk::WindowWindowClass::InputOutput,
        x: Some(geom.0),
        y: Some(geom.1),
        width: geom.2,
        height: geom.3,
        visual: Some(parent.visual()),
        ..Default::default()
    };
    let child = gtk::gdk::Window::new(Some(&parent), &attr);
    if !child.ensure_native() {
        child.destroy();
        return Err("child window is not native".into());
    }
    // Clicks, drags and the wheel fall through to the webview.
    child.input_shape_combine_region(&gtk::cairo::Region::create(), 0, 0);
    // GDK buffers requests; Vulkan's WSI asks the server about this window.
    child.display().sync();
    let Some(xid) = child.downcast_ref::<gdkx11::X11Window>().map(|w| w.xid()) else {
        child.destroy();
        return Err("not an X11 window".into());
    };

    ensure_thread(window.app_handle().clone());
    hook_keys(window);
    let instance = INSTANCE.get_or_init(|| wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle()));
    let display = window.display_handle().map_err(|e| e.to_string())?.as_raw();
    LAST_XID.store(xid, Ordering::Release);
    // SAFETY: the X display outlives the app, and the child window outlives
    // the surface: `remove` has the render thread drop the surface first and
    // only then destroys the window.
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(display),
            raw_window_handle: RawWindowHandle::Xlib(XlibWindowHandle::new(xid as std::ffi::c_ulong)),
        })
    };
    match surface {
        Ok(s) => Ok((child, s)),
        Err(e) => {
            child.destroy();
            Err(format!("surface: {e}"))
        }
    }
}

/* ---- Native keys ------------------------------------------------------------- */

thread_local! {
    static KEYS_HOOKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Hardware keycodes whose press was handled natively. Their release is
    /// swallowed too, so the page never sees a keyup without a keydown.
    static CONSUMED: RefCell<HashSet<u16>> = RefCell::new(HashSet::new());
}

/// Connect the key hooks on the toplevel, once. A handler connected to
/// `key-press-event` runs before GtkWindow's default handler, which is what
/// forwards the key to the focused WebKit view; stopping propagation here
/// means WebKit never sees it.
fn hook_keys(window: &tauri::WebviewWindow) {
    if KEYS_HOOKED.with(|h| h.replace(true)) {
        return;
    }
    let Ok(gw) = window.gtk_window() else { return };
    let app = window.app_handle().clone();
    gw.connect_key_press_event(move |_, ev| on_key_press(&app, ev));
    gw.connect_key_release_event(|_, ev| {
        if CONSUMED.with(|c| c.borrow_mut().remove(&ev.hardware_keycode())) {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
}

fn on_key_press(app: &tauri::AppHandle, ev: &gtk::gdk::EventKey) -> gtk::glib::Propagation {
    use gtk::gdk::ModifierType as M;
    use gtk::glib::translate::IntoGlib;
    use gtk::glib::Propagation;
    use tauri::Emitter;

    if FAILED.load(Ordering::Acquire) || ev.is_modifier() {
        return Propagation::Proceed;
    }
    let Some((pane, targets, session)) =
        FOCUS.lock().ok().and_then(|f| f.as_ref().map(|f| (f.pane.clone(), f.targets.clone(), f.session)))
    else {
        return Propagation::Proceed;
    };
    let state = ev.state();
    let keyval = ev.keyval();
    let ctrl = state.contains(M::CONTROL_MASK);
    // The Latin letter on this physical key, for Ctrl on a non-Latin layout.
    let latin = if ctrl && !keyval.to_unicode().is_some_and(|c| c.is_ascii_alphabetic()) {
        ev.window()
            .and_then(|w| gtk::gdk::Keymap::for_display(&w.display()))
            .and_then(|km| km.translate_keyboard_state(ev.hardware_keycode() as u32, M::empty(), 0))
            // Latin-1 keysyms are their code points.
            .and_then(|(k, ..)| char::from_u32(k))
            .filter(|c| c.is_ascii_alphabetic())
    } else {
        None
    };
    let key = crate::keys::Key {
        keyval: keyval.into_glib(),
        text: keyval.to_unicode(),
        latin,
        ctrl,
        shift: state.contains(M::SHIFT_MASK),
        alt: state.contains(M::MOD1_MASK),
        super_: state.intersects(M::SUPER_MASK | M::MOD4_MASK | M::META_MASK | M::HYPER_MASK),
    };
    let Ok(s) = registry().get(session) else { return Propagation::Proceed };
    let Some(bytes) = crate::keys::encode(&key, s.app_cursor()) else {
        return Propagation::Proceed;
    };

    #[cfg(feature = "latency-probe")]
    crate::latency_probe::mark(crate::latency_probe::Stage::Write);
    for id in targets {
        if let Ok(t) = registry().get(id) {
            // Same as `session_write`: a session that stopped reading reports
            // it; one keystroke is not worth an error dialog.
            let _ = t.write(&bytes);
        }
    }
    wake();
    CONSUMED.with(|c| c.borrow_mut().insert(ev.hardware_keycode()));
    // The page clears its selection on typing, as before. Off the hot path:
    // the bytes are already written.
    let _ = app.emit("term-native-key", pane);
    Propagation::Stop
}

fn destroy_child(pane: &str) {
    let child = CHILDREN.with(|c| c.borrow_mut().remove(pane));
    if let Some(ch) = child {
        destroy_window(ch.win);
    }
}

/// Destroy a child window we created with `gdk::Window::new`.
///
/// GDK 3's `gdk_window_destroy` ends with `g_object_unref`: it consumes the
/// reference `gdk_window_new` returned, which is the one our `gdk::Window`
/// owns. The binding passes a borrowed pointer, so dropping the handle
/// afterwards unrefs freed memory. That crashed the app (SIGSEGV in
/// libgdk-3) on every pane close, and on every split, which remounts a pane.
fn destroy_window(win: gtk::gdk::Window) {
    win.destroy();
    std::mem::forget(win);
}

/// From the render thread: the surface is configured, so the window may show.
fn on_ready(app: &tauri::AppHandle, pane: String) {
    let _ = app.run_on_main_thread(move || {
        CHILDREN.with(|c| {
            if let Some(ch) = c.borrow_mut().get_mut(&pane) {
                ch.ready = true;
                if ch.want_visible && show_synced(&ch.win) {
                    let _ = send(Msg::Exposed { pane: pane.clone() });
                }
            }
        });
    });
}

/// Map a hidden child window and wait until the server has. True if it was
/// hidden. The render thread presents on its own X connection, so without the
/// round trip its next present can reach the server before the map and be
/// dropped; with damage-only presents nothing would repair that.
fn show_synced(win: &gtk::gdk::Window) -> bool {
    if win.is_visible() {
        return false;
    }
    win.show();
    win.display().sync();
    true
}

fn on_detached(app: &tauri::AppHandle, pane: String) {
    let _ = app.run_on_main_thread(move || destroy_child(&pane));
}

/// From the render thread: no adapter or device. Everything goes back to the
/// canvas; `gpu_pane_set` fails from now on.
fn on_failed(app: &tauri::AppHandle, reason: &str) {
    fail(reason);
    clear_focus(None);
    let _ = app.run_on_main_thread(|| {
        CHILDREN.with(|c| {
            for (_, ch) in c.borrow_mut().drain() {
                destroy_window(ch.win);
            }
        });
    });
}

fn ensure_thread(app: tauri::AppHandle) {
    SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel(256);
        let spawned = std::thread::Builder::new().name("gpu-term".into()).spawn(move || run(rx, app));
        if let Err(e) = spawned {
            fail(format!("render thread: {e}"));
        }
        tx
    });
}

/* ---- Render thread ------------------------------------------------------------ */

/// See `Pane::exposed`.
const EXPOSE_FRAMES: u8 = 2;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    adapter: wgpu::Adapter,
    /// `Some(incremental)` while panes present damage-only through their own
    /// Vulkan swapchain (`opspilot_render::present`); `None` once that is
    /// unavailable or has failed, and panes use wgpu's surface.
    damage: Option<bool>,
}

struct Pane {
    /// Declared before `surface`: it owns the surface's swapchain and must be
    /// destroyed first.
    presenter: Option<Presenter>,
    surface: wgpu::Surface<'static>,
    config: Option<wgpu::SurfaceConfiguration>,
    frame: Frame,
    want: Want,
    /// Generation last drawn.
    seen: u64,
    redraw: bool,
    /// Frames that must still present the whole window, because it was just
    /// (re)shown and the server holds nothing of ours for it. Two, not one:
    /// in the nested test server, the first present after a map was dropped
    /// in about one case in three even after a round trip (the window's own
    /// pixels stayed stale), and a second, a frame later, always landed.
    exposed: u8,
    live: bool,
}

struct State {
    app: tauri::AppHandle,
    gpu: Option<Gpu>,
    glyphs: Option<(FontKey, Glyphs)>,
    panes: HashMap<String, Pane>,
    /// Per-frame scratch for rows that carry a prediction.
    scratch: (Vec<u32>, Vec<u16>),
}

fn run(rx: Receiver<Msg>, app: tauri::AppHandle) {
    let mut st = State { app, gpu: None, glyphs: None, panes: HashMap::new(), scratch: (Vec::new(), Vec::new()) };
    let mut next_frame = Instant::now();
    loop {
        let wait = next_frame.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
        let first = match rx.recv_timeout(wait) {
            Ok(m) => Some(m),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut woke = false;
        for msg in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match msg {
                Msg::Wake => woke = true,
                Msg::Output => {
                    // Cleared before drawing, so output that lands during the
                    // draw queues another one.
                    OUTPUT_PENDING.store(false, Ordering::Release);
                    woke = true;
                }
                other => st.handle(other),
            }
        }
        let now = Instant::now();
        if woke || now >= next_frame {
            st.draw_all();
            if now >= next_frame {
                next_frame = now + FRAME;
            }
        }
    }
}

impl State {
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Attach { pane, surface, want } => {
                if self.gpu.is_none() {
                    match init_gpu(&surface) {
                        Ok(g) => self.gpu = Some(g),
                        Err(why) => {
                            drop(surface);
                            self.panes.clear();
                            on_failed(&self.app, &why);
                            return;
                        }
                    }
                }
                let glyphs = self.glyphs_for(want.font);
                let frame = Frame::new(glyphs.fonts.metrics, Palette::default(), want.pad);
                let mut p = Pane {
                    presenter: None,
                    surface,
                    config: None,
                    frame,
                    want,
                    seen: 0,
                    redraw: true,
                    exposed: EXPOSE_FRAMES,
                    live: false,
                };
                if let Some(gpu) = &mut self.gpu {
                    configure(gpu, &mut p);
                }
                self.panes.insert(pane.clone(), p);
                on_ready(&self.app, pane);
            }
            Msg::Update { pane, want } => {
                let font_changed = self.glyphs.as_ref().is_some_and(|(k, _)| *k != want.font);
                if font_changed {
                    let m = self.glyphs_for(want.font).fonts.metrics;
                    for p in self.panes.values_mut() {
                        p.frame = Frame::new(m, Palette::default(), want.pad);
                        p.redraw = true;
                    }
                }
                let Some(p) = self.panes.get_mut(&pane) else { return };
                let resized = p.want.size != want.size;
                let changed = resized
                    || p.want.session != want.session
                    || p.want.focused != want.focused
                    || p.want.selection != want.selection
                    || (want.visible && !p.want.visible);
                if p.want.session != want.session || p.want.pad != want.pad {
                    let m = p.frame.metrics();
                    p.frame = Frame::new(m, Palette::default(), want.pad);
                }
                p.want = want;
                if resized {
                    if let Some(gpu) = &mut self.gpu {
                        configure(gpu, p);
                    }
                }
                p.redraw |= changed;
            }
            Msg::Exposed { pane } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.exposed = EXPOSE_FRAMES;
                    p.redraw = true;
                }
            }
            Msg::Detach { pane } => {
                // Dropping the Pane drops its surface, before the window goes.
                self.panes.remove(&pane);
                on_detached(&self.app, pane);
            }
            Msg::Wake | Msg::Output => {}
        }
    }

    /// The atlas for this font, replacing the old one if the size changed.
    fn glyphs_for(&mut self, font: FontKey) -> &mut Glyphs {
        if !self.glyphs.as_ref().is_some_and(|(k, _)| *k == font) {
            self.glyphs = Some((font, Glyphs::new(Fonts::new(font.px, font.line))));
        }
        &mut self.glyphs.as_mut().expect("just set").1
    }

    fn draw_all(&mut self) {
        let (Some(gpu), Some((_, glyphs))) = (self.gpu.as_mut(), self.glyphs.as_mut()) else { return };
        for (name, pane) in self.panes.iter_mut() {
            if !pane.want.visible {
                continue;
            }
            if draw(gpu, glyphs, pane, &mut self.scratch) && !pane.live {
                pane.live = true;
                set_live(name, true);
            }
        }
    }
}

fn init_gpu(surface: &wgpu::Surface<'static>) -> Result<Gpu, String> {
    let instance = INSTANCE.get().ok_or("no wgpu instance")?;
    // `OPSPILOT_GPU_DAMAGE=0` keeps wgpu's own surface, for comparison.
    let want_damage = std::env::var("OPSPILOT_GPU_DAMAGE").as_deref() != Ok("0");
    let (adapter, device, queue, damage) = opspilot_render::gpu::for_surface_damage(instance, surface, want_damage)
        .ok_or("no adapter can present to the child window")?;
    let format = Renderer::surface_format(&surface.get_capabilities(&adapter)).ok_or("surface reports no formats")?;
    let renderer = Renderer::new(&device, &queue, format);
    let info = adapter.get_info();
    eprintln!("gpu terminal: {} ({:?}), {format:?}", info.name, info.backend);
    Ok(Gpu { device, queue, renderer, adapter, damage })
}

/// Draw only what changed into the pane's canvas and present that region.
/// `Ok(false)`: not shown, draw again.
fn draw_damage(gpu: &mut Gpu, glyphs: &mut Glyphs, p: &mut Pane) -> Result<bool, String> {
    let Some(pr) = p.presenter.as_mut() else { return Ok(false) };
    let need = pr.begin(p.want.size)?;
    let size = pr.size();
    let full = PixelRect::full(size);
    let exposed = p.exposed > 0;
    p.exposed = p.exposed.saturating_sub(1);
    let region = match (need, p.frame.take_damage(size)) {
        (Need::Full, _) => None,
        (Need::Damage, Some(r)) => (r != full).then_some(r),
        // Nothing visible moved: nothing to draw, and nothing to show unless
        // the window lost its contents.
        (Need::Damage, None) if !exposed => return Ok(true),
        // The canvas is current; only the window needs it again.
        (Need::Damage, None) => Some(PixelRect { x: 0, y: 0, w: 0, h: 0 }),
    };
    if !region.is_some_and(|r| r.w == 0 || r.h == 0) {
        let bg = p.frame.palette.bg;
        let instances = p.frame.instances(glyphs);
        gpu.renderer.upload(glyphs);
        let view = pr.canvas().ok_or("no canvas")?;
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        gpu.renderer.draw_damage(&mut enc, view, size, bg, instances, region);
        gpu.queue.submit([enc.finish()]);
    }
    #[cfg(feature = "latency-probe")]
    {
        if crate::latency_probe::gpu_wait() {
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        }
        crate::latency_probe::mark(crate::latency_probe::Stage::Gpu);
    }
    let shown = pr.present(if exposed { full } else { region.unwrap_or(full) })?;
    #[cfg(feature = "latency-probe")]
    crate::latency_probe::mark(crate::latency_probe::Stage::Present);
    Ok(shown)
}

fn configure(gpu: &mut Gpu, p: &mut Pane) {
    p.redraw = true;
    if p.presenter.is_some() {
        // The presenter follows the size itself (`Presenter::begin`).
        return;
    }
    if let Some(incremental) = gpu.damage {
        match Presenter::new(&gpu.device, &gpu.queue, &p.surface, gpu.renderer.format(), incremental) {
            Ok(pr) => {
                eprintln!("gpu terminal: damage-only present (incremental regions: {incremental})");
                p.presenter = Some(pr);
                p.frame.invalidate();
                return;
            }
            Err(why) => {
                eprintln!("gpu terminal: damage-only present unavailable ({why}); using the wgpu surface");
                gpu.damage = None;
            }
        }
    }
    let caps = p.surface.get_capabilities(&gpu.adapter);
    // Immediate measured 3.86 ms p50 into a child window, Mailbox 14 ms
    // (spike). In the app, keystroke to pixel: Immediate 5.0 ms, Mailbox 15.3,
    // Fifo 14.4. Under a compositor the window is redirected, so Immediate does
    // not tear; without one it may.
    let mode = [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
        .into_iter()
        .find(|m| caps.present_modes.contains(m))
        .unwrap_or(wgpu::PresentMode::Fifo);
    #[cfg(feature = "latency-probe")]
    let mode = crate::latency_probe::present_mode().filter(|m| caps.present_modes.contains(m)).unwrap_or(mode);
    let alpha = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        wgpu::CompositeAlphaMode::Opaque
    } else {
        caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto)
    };
    let Some(mut config) = p.surface.get_default_config(&gpu.adapter, p.want.size.0, p.want.size.1) else {
        return;
    };
    config.format = gpu.renderer.format();
    config.present_mode = mode;
    config.alpha_mode = alpha;
    config.desired_maximum_frame_latency = 1;
    p.surface.configure(&gpu.device, &config);
    p.config = Some(config);
    p.redraw = true;
}

/// Draw one pane if anything changed. True if a frame was presented.
fn draw(gpu: &mut Gpu, glyphs: &mut Glyphs, p: &mut Pane, scratch: &mut (Vec<u32>, Vec<u16>)) -> bool {
    let Ok(session) = registry().get(p.want.session) else { return false };
    let generation = session.generation();
    if generation == p.seen && !p.redraw {
        return false;
    }
    #[cfg(feature = "latency-probe")]
    crate::latency_probe::mark(crate::latency_probe::Stage::Draw);
    let frame = &mut p.frame;
    let focused = p.want.focused;
    let built = session.paint(|v| {
        let grid = v.screen.grid();
        let (rows, cols) = (grid.rows(), grid.cols());
        frame.resize(rows, cols);
        if v.full {
            frame.invalidate();
        } else {
            for &r in v.dirty {
                frame.invalidate_row(r);
            }
        }
        // Twice at most: if the atlas fills and clears part-way through, the
        // rows built before the clear point at recycled slots.
        for _ in 0..2 {
            let epoch = glyphs.epoch;
            let stale: Vec<usize> = frame.stale_rows(glyphs).collect();
            for r in stale {
                let (g, s) = v.row(r, &mut scratch.0, &mut scratch.1);
                frame.set_row(glyphs, r, g, s, v.screen.styles());
            }
            if glyphs.epoch == epoch {
                break;
            }
        }
        let cursor = (v.cursor_visible && v.cursor.row < rows && cols > 0).then(|| {
            let col = v.cursor.col.min(cols - 1);
            let (g, s) = v.row(v.cursor.row, &mut scratch.0, &mut scratch.1);
            CursorDraw {
                row: v.cursor.row,
                col,
                glyph: g.get(col).copied().unwrap_or(b' ' as u32),
                style: s.get(col).map(|&id| v.screen.styles().get(id)).unwrap_or_default(),
                focused,
            }
        });
        frame.set_cursor(cursor);
    });
    if built.is_err() {
        return false;
    }
    p.frame.set_selection(p.want.selection);

    if p.presenter.is_some() {
        return match draw_damage(gpu, glyphs, p) {
            Ok(shown) => {
                p.seen = generation;
                // Not shown: the window changed under the frame. The next one
                // is full. Or an expose still owes a whole-window present.
                p.redraw = !shown || p.exposed > 0;
                shown
            }
            Err(why) => {
                eprintln!("gpu terminal: damage-only present failed ({why}); using the wgpu surface");
                p.presenter = None;
                gpu.damage = None;
                p.frame.invalidate();
                configure(gpu, p);
                false
            }
        };
    }
    // wgpu's surface repaints everything; damage only matters to the presenter.
    let _ = p.frame.take_damage(p.want.size);

    let tex = match p.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            configure(gpu, p);
            return false;
        }
        _ => {
            p.redraw = true;
            return false;
        }
    };
    let bg = p.frame.palette.bg;
    let size = p.config.as_ref().map(|c| (c.width, c.height)).unwrap_or(p.want.size);
    let instances = p.frame.instances(glyphs);
    gpu.renderer.upload(glyphs);
    let view = tex.texture.create_view(&Default::default());
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    gpu.renderer.draw(&mut enc, &view, size, bg, instances);
    gpu.queue.submit([enc.finish()]);
    #[cfg(feature = "latency-probe")]
    {
        if crate::latency_probe::gpu_wait() {
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        }
        crate::latency_probe::mark(crate::latency_probe::Stage::Gpu);
    }
    gpu.queue.present(tex);
    #[cfg(feature = "latency-probe")]
    crate::latency_probe::mark(crate::latency_probe::Stage::Present);
    p.seen = generation;
    p.redraw = false;
    true
}
