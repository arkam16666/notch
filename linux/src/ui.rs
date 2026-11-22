//! The live window: a transparent strip pinned to the top centre of the
//! screen, with only the island itself clickable.
//!
//! - Wayland compositors with layer-shell (KDE Plasma, Hyprland, Sway, …):
//!   an overlay layer anchored to the top edge.
//! - X11: a "dock" window (no decorations, always on top, on every
//!   workspace) moved to the top centre.
//! - Wayland without layer-shell (GNOME): re-launch on XWayland and do the
//!   X11 thing.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::draw::{self, WIN_H, WIN_W};
use crate::model::{Art, Banner, Detail, Hit, Model, Page, Timer, TIMER_PRESETS};
use crate::{claude_api, config, mpris, net, status, sys};

struct Channels {
    mpris_rx: Receiver<mpris::Msg>,
    mpris_tx: Sender<mpris::Cmd>,
    status_rx: Receiver<Vec<status::Session>>,
    limits_rx: Receiver<Result<claude_api::Limits, String>>,
    limits_poke: Sender<()>,
    sys_rx: Receiver<sys::Event>,
    net_tx: Sender<net::Req>,
    net_rx: Receiver<net::Resp>,
}

struct App {
    model: RefCell<Model>,
    ch: Channels,
    area: gtk::DrawingArea,
    window: gtk::ApplicationWindow,
    /// leaving the open island collapses it after a moment
    left_at: RefCell<Option<Instant>>,
    last_frame: RefCell<Instant>,
    last_draw: RefCell<Instant>,
    input_rect: RefCell<(i32, i32, i32, i32)>,
    low_warned: RefCell<u8>,
    on_ac: RefCell<Option<bool>>,
    /// Wi-Fi password field (shown over the island when needed)
    password: gtk::PasswordEntry,
    layer_shell: bool,
    /// slider drags: throttle the commands
    last_slider: RefCell<Instant>,
    last_poll: RefCell<Instant>,
    /// screen-edge glow window (Wayland layer-shell)
    edge: RefCell<Option<(gtk::Window, gtk::DrawingArea)>>,
    /// see-through full-screen window under the open island: a click on it
    /// (anywhere outside the island) closes the island
    catcher: RefCell<Option<gtk::Window>>,
}

pub fn run(args: Vec<String>) {
    let app = gtk::Application::builder()
        .application_id("io.github.arkam16666.Notch")
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_activate(build);
    // `notch --settings` (here or from a second launch): open the settings
    app.connect_command_line(|app, cmd| {
        let settings = cmd.arguments().iter().any(|a| a.to_str() == Some("--settings"));
        build(app);
        if settings {
            crate::settings::show(app);
        }
        0.into()
    });
    app.run_with_args(&args);
}

/// Logical pixels to keep clear at the top on X11: GNOME's top bar sits
/// above every app window, so there the island hangs just below it.
fn top_offset() -> i32 {
    if let Some(v) = std::env::var("NOTCH_TOP_OFFSET").ok().and_then(|v| v.parse().ok()) {
        return v;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    if desktop.contains("GNOME") || desktop.contains("UBUNTU") { 34 } else { 0 }
}

fn is_x11() -> bool {
    gdk::Display::default().is_some_and(|d| d.type_().name().contains("X11"))
}

fn build(app: &gtk::Application) {
    if app.windows().iter().any(|w| w.widget_name() == "notch-island") {
        return; // already running
    }
    let x11 = is_x11();
    let layer_shell = !x11 && gtk4_layer_shell::is_supported();
    let wayland_without_layer_shell = !x11 && !layer_shell;
    if wayland_without_layer_shell && std::env::var_os("NOTCH_NO_XWAYLAND").is_none() {
        // GNOME: no layer-shell → run on XWayland, where we can be a dock
        eprintln!("notch: no layer-shell here (GNOME?) — restarting on XWayland");
        if let Ok(exe) = std::env::current_exe() {
            let mut cmd = std::process::Command::new(exe);
            cmd.env("GDK_BACKEND", "x11").env("NOTCH_NO_XWAYLAND", "1");
            // started without X variables (e.g. from a systemd unit): find
            // GNOME's XWayland display and its auth file ourselves
            if std::env::var_os("DISPLAY").is_none() {
                cmd.env("DISPLAY", ":0");
            }
            if std::env::var_os("XAUTHORITY").is_none() {
                let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
                if let Some(auth) = std::fs::read_dir(&dir).ok().and_then(|d| {
                    d.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".mutter-Xwaylandauth")))
                }) {
                    cmd.env("XAUTHORITY", auth);
                }
            }
            let _ = cmd.spawn();
        }
        app.quit();
        return;
    }

    let window = gtk::ApplicationWindow::new(app);
    window.set_widget_name("notch-island");
    window.set_title(Some("Notch"));
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_default_size(WIN_W as i32, WIN_H as i32);
    let css = gtk::CssProvider::new();
    // only the island and the edge glow are see-through (not the settings)
    css.load_from_data("window#notch-island, window#notch-edge, window#notch-catcher { background: transparent; box-shadow: none; }");
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }

    let area = gtk::DrawingArea::new();
    area.set_content_width(WIN_W as i32);
    area.set_content_height(WIN_H as i32);
    // the Wi-Fi password field floats over the drawing
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&area));
    let password = gtk::PasswordEntry::new();
    password.set_show_peek_icon(true);
    password.set_halign(gtk::Align::Start);
    password.set_valign(gtk::Align::Start);
    password.set_visible(false);
    overlay.add_overlay(&password);
    window.set_child(Some(&overlay));

    if layer_shell {
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Top, true);
        window.set_exclusive_zone(-1); // sit on the very edge, over panels
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_namespace(Some("notch"));
    } else {
        if top_offset() > 0 {
            draw::FLOATING.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        window.connect_realize(|w| x11_dock(w));
    }

    // ---- background threads ----
    let (mpris_tx_msg, mpris_rx) = mpsc::channel();
    let (mpris_tx, mpris_cmd_rx) = mpsc::channel();
    std::thread::spawn(move || mpris::run(mpris_tx_msg, mpris_cmd_rx));
    let (status_tx, status_rx) = mpsc::channel();
    std::thread::spawn(move || status::run(status_tx));
    let (limits_tx, limits_rx) = mpsc::channel();
    let (limits_poke, poke_rx) = mpsc::channel();
    std::thread::spawn(move || claude_api::run(limits_tx, poke_rx));
    let (sys_tx, sys_rx) = mpsc::channel();
    let clip_handled = sys::start(sys_tx);
    let (net_tx, net_req_rx) = mpsc::channel();
    let (net_resp_tx, net_rx) = mpsc::channel();
    std::thread::spawn(move || net::run(net_req_rx, net_resp_tx));
    let _ = net_tx.send(net::Req::ListSinks);

    let me = Rc::new(App {
        model: RefCell::new(Model { battery: sys::battery(), cfg: config::get(), ..Default::default() }),
        ch: Channels { mpris_rx, mpris_tx, status_rx, limits_rx, limits_poke, sys_rx, net_tx, net_rx },
        area: area.clone(),
        window: window.clone(),
        left_at: RefCell::new(None),
        last_frame: RefCell::new(Instant::now()),
        last_draw: RefCell::new(Instant::now()),
        input_rect: RefCell::new((0, 0, 0, 0)),
        low_warned: RefCell::new(100),
        on_ac: RefCell::new(sys::battery().map(|b| b.1)),
        password: password.clone(),
        layer_shell,
        last_slider: RefCell::new(Instant::now()),
        last_poll: RefCell::new(Instant::now()),
        edge: RefCell::new(None),
        catcher: RefCell::new(None),
    });
    // outside-click catcher (needs see-through windows: Wayland, or a
    // compositing X11 — otherwise leaving the island closes it instead)
    let catcher = if layer_shell || gdk::Display::default().is_some_and(|d| d.is_composited()) {
        Some(make_catcher(app, layer_shell))
    } else {
        None
    };
    if let Some(c) = &catcher {
        let weak = Rc::downgrade(&me);
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(me) = weak.upgrade() {
                me.set_expanded(false);
            }
        });
        c.add_controller(click);
    }
    *me.catcher.borrow_mut() = catcher;
    {
        let me = me.clone();
        password.connect_activate(move |_| me.join_with_password());
    }
    {
        // Esc in the password field → cancel
        let keys = gtk::EventControllerKey::new();
        let me = me.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                me.model.borrow_mut().password_for = None;
                me.sync_password();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        password.add_controller(keys);
    }

    // X11 (or Wayland without data-control): GTK's own clipboard signal
    if !clip_handled {
        if let Some(display) = gdk::Display::default() {
            let me2 = me.clone();
            display.clipboard().connect_changed(move |cb| {
                let me3 = me2.clone();
                cb.read_text_async(None::<&gtk::gio::Cancellable>, move |r| {
                    if let Ok(Some(text)) = r {
                        me3.add_clip(text.to_string());
                    }
                });
            });
        }
    }

    {
        let me = me.clone();
        area.set_draw_func(move |_, cr, _, _| draw::draw(cr, &me.model.borrow()));
    }

    // ---- input ----
    let click = gtk::GestureClick::new();
    click.set_button(0);
    {
        let me = me.clone();
        click.connect_pressed(move |g, _n, x, y| {
            if g.current_button() == 3 {
                me.quit_menu(x, y);
            } else {
                me.clicked(x, y);
            }
        });
    }
    area.add_controller(click);
    let motion = gtk::EventControllerMotion::new();
    {
        let me = me.clone();
        motion.connect_enter(move |_, x, y| me.moved(Some((x, y))));
    }
    {
        let me = me.clone();
        motion.connect_motion(move |_, x, y| me.moved(Some((x, y))));
    }
    {
        let me = me.clone();
        motion.connect_leave(move |_| me.moved(None));
    }
    area.add_controller(motion);
    let drag = gtk::GestureDrag::new();
    {
        let me = me.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let h = draw::hit(&me.model.borrow(), x, y);
            if let Some(h @ (Hit::Volume(_) | Hit::Brightness(_))) = h {
                me.model.borrow_mut().dragging = Some(h);
            }
        });
    }
    {
        let me = me.clone();
        drag.connect_drag_update(move |g, dx, _| {
            let which = me.model.borrow().dragging;
            let (Some(which), Some((sx, _))) = (which, g.start_point()) else { return };
            let h = draw::slider_value(&me.model.borrow(), which, sx + dx);
            if let Some(h) = h {
                me.slider(h, false);
            }
        });
    }
    {
        let me = me.clone();
        drag.connect_drag_end(move |g, dx, _| {
            let which = me.model.borrow_mut().dragging.take();
            if let (Some(which), Some((sx, _))) = (which, g.start_point()) {
                let h = draw::slider_value(&me.model.borrow(), which, sx + dx);
                if let Some(h) = h {
                    me.slider(h, true);
                }
            }
        });
    }
    area.add_controller(drag);

    window.present();

    // test hook: NOTCH_TEST_CLICKS="x,y@secs;…" (window coordinates) — lets
    // the headless test drive real clicks through the same code path
    if let Ok(spec) = std::env::var("NOTCH_TEST_CLICKS") {
        for item in spec.split(';') {
            let parsed = item.split_once('@').and_then(|(xy, t)| {
                let (x, y) = xy.split_once(',')?;
                Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?, t.trim().parse::<f64>().ok()?))
            });
            if let Some((x, y, t)) = parsed {
                let me = me.clone();
                glib::timeout_add_local_once(Duration::from_secs_f64(t), move || {
                    me.moved(Some((x, y)));
                    me.clicked(x, y);
                });
            }
        }
    }

    // ---- frame loop ----
    let me2 = me.clone();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        me2.tick();
        glib::ControlFlow::Continue
    });
}

thread_local! {
    /// X11 only: (connection, window, scale) — to cut the window to the
    /// island's shape when there's no compositor (no transparency).
    static X11: RefCell<Option<(x11rb::rust_connection::RustConnection, u32, i32)>> = const { RefCell::new(None) };
}

/// No compositor on X11 → transparent pixels would show as black: clip the
/// window itself to the island rectangle (XShape).
fn x11_shape(r: (i32, i32, i32, i32)) {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
    use x11rb::protocol::xproto::{ClipOrdering, Rectangle};
    X11.with(|x| {
        if let Some((conn, xid, scale)) = x.borrow().as_ref() {
            let k = *scale;
            let rect = Rectangle { x: (r.0 * k) as i16, y: (r.1 * k) as i16, width: (r.2 * k) as u16, height: (r.3 * k) as u16 };
            let _ = conn.shape_rectangles(SO::SET, SK::BOUNDING, ClipOrdering::UNSORTED, *xid, 0, 0, &[rect]);
            let _ = conn.flush();
        }
    });
}

thread_local! {
    static X11_EDGE: RefCell<Vec<x11rb::rust_connection::RustConnection>> = const { RefCell::new(Vec::new()) };
}

/// The outside-click catcher: full screen, see-through, under the island.
fn make_catcher(app: &gtk::Application, layer_shell: bool) -> gtk::Window {
    let w = gtk::Window::new();
    w.set_application(Some(app));
    w.set_decorated(false);
    w.set_widget_name("notch-catcher");
    let area = gtk::DrawingArea::new(); // draws nothing, just takes the click
    area.set_hexpand(true);
    area.set_vexpand(true);
    w.set_child(Some(&area));
    if layer_shell {
        w.init_layer_shell();
        w.set_layer(Layer::Top); // the island is on Overlay, above this
        for e in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            w.set_anchor(e, true);
        }
        w.set_exclusive_zone(-1);
        w.set_keyboard_mode(KeyboardMode::None);
        w.set_namespace(Some("notch-catcher"));
    } else {
        let (sw, sh) = gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_then(|m| m.downcast::<gdk::Monitor>().ok())
            .map(|m| (m.geometry().width(), m.geometry().height()))
            .unwrap_or((1920, 1080));
        w.set_default_size(sw, sh);
        w.set_resizable(false);
        w.connect_realize(|w| x11_overlay(w, false));
        // keep the island above the catcher
        w.connect_map(|_| x11_raise_island());
    }
    w
}

/// X11: put the island window back on top (after the catcher appears).
fn x11_raise_island() {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _, StackMode};
    X11.with(|x| {
        if let Some((conn, xid, _)) = x.borrow().as_ref() {
            let _ = conn.configure_window(*xid, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
            let _ = conn.flush();
        }
    });
}

/// X11: a full-screen dock at 0,0 over everything — the edge glow
/// (`click_through`: empty XShape input region) or the click catcher.
fn x11_overlay(w: &gtk::Window, click_through: bool) {
    use gdk4_x11::X11Surface;
    use x11rb::connection::Connection as _;
    use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
    use x11rb::protocol::xproto::{AtomEnum, ClipOrdering, ConfigureWindowAux, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;
    let Some(surface) = w.surface() else { return };
    let Ok(xs) = surface.downcast::<X11Surface>() else { return };
    let xid = xs.xid() as u32;
    let Ok((conn, _)) = x11rb::connect(None) else { return };
    let atom = |name: &str| conn.intern_atom(false, name.as_bytes()).ok().and_then(|c| c.reply().ok()).map(|r| r.atom);
    if let (Some(t), Some(dock), Some(st), Some(above), Some(sticky), Some(stb), Some(spg)) = (
        atom("_NET_WM_WINDOW_TYPE"),
        atom("_NET_WM_WINDOW_TYPE_DOCK"),
        atom("_NET_WM_STATE"),
        atom("_NET_WM_STATE_ABOVE"),
        atom("_NET_WM_STATE_STICKY"),
        atom("_NET_WM_STATE_SKIP_TASKBAR"),
        atom("_NET_WM_STATE_SKIP_PAGER"),
    ) {
        let _ = conn.change_property32(PropMode::REPLACE, xid, t, AtomEnum::ATOM, &[dock]);
        let _ = conn.change_property32(PropMode::REPLACE, xid, st, AtomEnum::ATOM, &[above, sticky, stb, spg]);
    }
    if click_through {
        let _ = conn.shape_rectangles(SO::SET, SK::INPUT, ClipOrdering::UNSORTED, xid, 0, 0, &[]);
    }
    let _ = conn.configure_window(xid, &ConfigureWindowAux::new().x(0).y(0));
    let _ = conn.flush();
    let idx = X11_EDGE.with(|c| c.borrow().len());
    w.connect_map(move |_| {
        X11_EDGE.with(|c| {
            if let Some(conn) = c.borrow().get(idx) {
                let _ = conn.configure_window(xid, &ConfigureWindowAux::new().x(0).y(0));
                if click_through {
                    let _ = conn.shape_rectangles(SO::SET, SK::INPUT, ClipOrdering::UNSORTED, xid, 0, 0, &[]);
                }
                let _ = conn.flush();
            }
        });
    });
    X11_EDGE.with(|c| c.borrow_mut().push(conn));
}

/// X11: make the window a dock (no frame, above everything, all workspaces)
/// at the top centre of the screen.
fn x11_dock(w: &gtk::ApplicationWindow) {
    use gdk4_x11::X11Surface;
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConfigureWindowAux, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;

    let Some(surface) = w.surface() else { return };
    let Ok(xs) = surface.downcast::<X11Surface>() else { return };
    let xid = xs.xid() as u32;
    let scale = xs.scale_factor().max(1) as u32;
    let Ok((conn, screen_num)) = x11rb::connect(None) else { return };
    let screen_w = conn.setup().roots[screen_num].width_in_pixels as u32;
    let atom = |name: &str| conn.intern_atom(false, name.as_bytes()).ok().and_then(|c| c.reply().ok()).map(|r| r.atom);
    let (Some(wtype), Some(dock), Some(state), Some(above), Some(sticky), Some(skip_tb), Some(skip_pg)) = (
        atom("_NET_WM_WINDOW_TYPE"),
        atom("_NET_WM_WINDOW_TYPE_DOCK"),
        atom("_NET_WM_STATE"),
        atom("_NET_WM_STATE_ABOVE"),
        atom("_NET_WM_STATE_STICKY"),
        atom("_NET_WM_STATE_SKIP_TASKBAR"),
        atom("_NET_WM_STATE_SKIP_PAGER"),
    ) else {
        return;
    };
    let _ = conn.change_property32(PropMode::REPLACE, xid, wtype, AtomEnum::ATOM, &[dock]);
    let _ = conn.change_property32(PropMode::REPLACE, xid, state, AtomEnum::ATOM, &[above, sticky, skip_tb, skip_pg]);
    let win_w = WIN_W as u32 * scale;
    let x = screen_w.saturating_sub(win_w) / 2;
    let y = top_offset() * scale as i32;
    let _ = conn.configure_window(xid, &ConfigureWindowAux::new().x(x as i32).y(y));
    let _ = conn.flush();
    // re-assert the position once mapped (some window managers place it)
    w.connect_map(move |_| {
        X11.with(|c| {
            if let Some((conn, xid, _)) = c.borrow().as_ref() {
                let _ = conn.configure_window(*xid, &ConfigureWindowAux::new().x(x as i32).y(y));
                let _ = conn.flush();
            }
        });
    });
    X11.with(|c| *c.borrow_mut() = Some((conn, xid, scale as i32)));
}

/// Decode artwork bytes into a 256 px square cairo surface + accent colour.
fn decode_art(bytes: &[u8]) -> Option<Art> {
    let tex = gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok()?;
    let (w, h) = (tex.width(), tex.height());
    let stride = w as usize * 4;
    let mut data = vec![0u8; stride * h as usize];
    tex.download(&mut data, stride);
    let src = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, stride as i32).ok()?;
    let out = cairo::ImageSurface::create(cairo::Format::ARgb32, 256, 256).ok()?;
    {
        let cr = cairo::Context::new(&out).ok()?;
        // centre-crop to a square (video thumbnails are 16:9)
        let side = w.min(h) as f64;
        let k = 256.0 / side;
        cr.scale(k, k);
        cr.set_source_surface(&src, -(w as f64 - side) / 2.0, -(h as f64 - side) / 2.0).ok()?;
        cr.source().set_filter(cairo::Filter::Good);
        cr.paint().ok()?;
    }
    let accent = accent_of(&out).unwrap_or((0.8, 0.5, 1.0));
    Some(Art { surface: out, accent })
}

/// A vivid colour from the artwork (average of the most saturated pixels).
pub fn accent_of(s: &cairo::ImageSurface) -> Option<(f64, f64, f64)> {
    let mut s = s.clone();
    let (w, h, stride) = (s.width() as usize, s.height() as usize, s.stride() as usize);
    let data = s.data().ok()?;
    let mut px: Vec<(f64, f64, f64, f64)> = Vec::new();
    for y in (0..h).step_by(8) {
        for x in (0..w).step_by(8) {
            let i = y * stride + x * 4;
            let (b, g, r) = (data[i] as f64 / 255.0, data[i + 1] as f64 / 255.0, data[i + 2] as f64 / 255.0);
            let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
            let sat = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
            px.push((sat * mx, r, g, b));
        }
    }
    px.sort_by(|a, b| b.0.total_cmp(&a.0));
    let top = &px[..(px.len() / 5).max(1)];
    let n = top.len() as f64;
    let (r, g, b) = top.iter().fold((0.0, 0.0, 0.0), |acc, p| (acc.0 + p.1, acc.1 + p.2, acc.2 + p.3));
    let (r, g, b) = (r / n, g / n, b / n);
    // lift dark colours so they read on black
    let mx = r.max(g).max(b).max(0.01);
    let k = (0.85 / mx).min(2.5);
    Some(((r * k).min(1.0), (g * k).min(1.0), (b * k).min(1.0)))
}

impl App {
    fn set_expanded(&self, e: bool) {
        if let Some(c) = self.catcher.borrow().as_ref() {
            c.set_visible(e);
        }
        let mut m = self.model.borrow_mut();
        if m.expanded == e {
            return;
        }
        if e {
            let _ = self.ch.net_tx.send(net::Req::Refresh);
        }
        m.expanded = e;
        if !e {
            m.out_open = false;
            m.password_for = None;
        }
        if e {
            m.banner = None;
            if m.page == Page::Claude {
                let _ = self.ch.limits_poke.send(());
            }
        }
    }

    fn moved(&self, p: Option<(f64, f64)>) {
        if std::env::var_os("NOTCH_DEBUG").is_some() {
            eprintln!("notch: pointer {p:?}");
        }
        let mut m = self.model.borrow_mut();
        match p {
            Some((x, y)) => {
                let h = draw::hit(&m, x, y);
                m.hover = h.is_some();
                m.hover_hit = h.filter(|h| *h != Hit::Island);
                *self.left_at.borrow_mut() = if m.hover { None } else { Some(Instant::now()) };
            }
            None => {
                m.hover = false;
                m.hover_hit = None;
                *self.left_at.borrow_mut() = Some(Instant::now());
            }
        }
    }

    fn clicked(&self, x: f64, y: f64) {
        if std::env::var_os("NOTCH_DEBUG").is_some() {
            eprintln!("notch: click {x:.0},{y:.0}");
        }
        let h = draw::hit(&self.model.borrow(), x, y);
        let Some(h) = h else { return };
        let expanded = self.model.borrow().expanded;
        if !expanded {
            return self.set_expanded(true);
        }
        let mut collapse = false;
        let mut m = self.model.borrow_mut();
        // the switch in a list's header = that tile
        let h = match h {
            Hit::DetailSwitch if m.detail == Some(Detail::Wifi) => Hit::TileWifi,
            Hit::DetailSwitch => Hit::TileBt,
            other => other,
        };
        match h {
            // a click on the empty header collapses again
            Hit::Island => collapse = y < 36.0,
            Hit::Tab(p) => {
                m.page = p;
                if p == Page::Claude {
                    let _ = self.ch.limits_poke.send(());
                }
            }
            Hit::Prev => {
                let _ = self.ch.mpris_tx.send(mpris::Cmd::Previous);
            }
            Hit::Next => {
                let _ = self.ch.mpris_tx.send(mpris::Cmd::Next);
            }
            Hit::Play => {
                let since = m.track_at.elapsed().as_secs_f64();
                if let Some(t) = m.track.as_mut() {
                    // optimistic: flip now, the next poll confirms
                    t.position += if t.playing { since } else { 0.0 };
                    t.playing = !t.playing;
                }
                m.track_at = Instant::now();
                let _ = self.ch.mpris_tx.send(mpris::Cmd::PlayPause);
            }
            Hit::Seek(f) => {
                if let Some(t) = m.track.as_mut() {
                    t.position = f * t.length;
                    let _ = self.ch.mpris_tx.send(mpris::Cmd::Seek(t.position));
                }
                m.track_at = Instant::now();
            }
            Hit::TimerPreset(i) => {
                m.timer_last = TIMER_PRESETS[i];
                m.timer = Some(Timer::new(TIMER_PRESETS[i] as f64 * 60.0));
                m.timer_done = None;
            }
            Hit::TimerToggle => {
                let last = m.timer_last;
                m.timer = Some(match m.timer {
                    Some(t) if t.running() => Timer { left: t.remaining(), started: None, ..t },
                    Some(t) => Timer { started: Some(Instant::now()), ..t },
                    None => Timer::new(last as f64 * 60.0),
                });
                m.timer_done = None;
            }
            Hit::TimerReset => m.timer = None,
            Hit::TimerPlus => {
                let t = m.timer.get_or_insert(Timer { total: 0.0, left: 0.0, started: Some(Instant::now()) });
                t.total += 60.0;
                t.left += 60.0;
            }
            Hit::ClipClear => m.clip.clear(),
            Hit::TileWifi => {
                let on = m.controls.wifi != Some(true);
                m.controls.wifi = Some(on);
                if !on {
                    m.controls.wifi_ssid = None;
                    m.nets.clear();
                } else if m.detail == Some(Detail::Wifi) {
                    m.scanning = true;
                    let _ = self.ch.net_tx.send(net::Req::ScanWifi);
                }
                let _ = self.ch.net_tx.send(net::Req::SetWifi(on));
            }
            Hit::TileBt => {
                let on = m.controls.bluetooth != Some(true);
                m.controls.bluetooth = Some(on);
                if !on {
                    m.controls.bt_device = None;
                }
                let _ = self.ch.net_tx.send(net::Req::SetBt(on));
                if m.detail == Some(Detail::Bluetooth) {
                    let _ = self.ch.net_tx.send(net::Req::ListBt);
                }
            }
            Hit::TileDark => {
                let on = m.controls.dark != Some(true);
                m.controls.dark = Some(on);
                let _ = self.ch.net_tx.send(net::Req::SetDark(on));
            }
            Hit::More(d) => {
                m.detail = Some(d);
                m.join_failed = None;
                match d {
                    Detail::Wifi => {
                        m.scanning = true;
                        let _ = self.ch.net_tx.send(net::Req::ScanWifi);
                    }
                    Detail::Bluetooth => {
                        let _ = self.ch.net_tx.send(net::Req::ListBt);
                    }
                }
            }
            Hit::Back => {
                m.detail = None;
                m.password_for = None;
            }
            Hit::NetRow(i) => {
                if let Some(n) = m.nets.get(i).cloned() {
                    if !n.current && m.joining.is_none() {
                        m.join_failed = None;
                        if n.known || !n.secure {
                            m.joining = Some(n.ssid.clone());
                            m.password_for = None;
                            let _ = self.ch.net_tx.send(net::Req::Join { ssid: n.ssid, password: None });
                        } else {
                            m.password_for = Some(n.ssid);
                        }
                    }
                }
            }
            Hit::Join => {
                drop(m);
                self.join_with_password();
                return;
            }
            Hit::BtRow(i) => {
                if let Some(d) = m.bt_devs.get(i).cloned() {
                    if m.bt_busy.is_none() {
                        m.bt_busy = Some(d.mac.clone());
                        let _ = self.ch.net_tx.send(net::Req::BtConnect { mac: d.mac, connect: !d.connected });
                    }
                }
            }
            Hit::Volume(_) | Hit::Brightness(_) => {
                drop(m);
                self.slider(h, true);
                return;
            }
            Hit::OutBtn => {
                m.out_open = true;
                let _ = self.ch.net_tx.send(net::Req::ListSinks);
            }
            Hit::OutBack => m.out_open = false,
            Hit::DetailSwitch => {} // mapped to a tile above
            Hit::OutRow(i) => {
                if let Some(s) = m.sinks.get(i).cloned() {
                    for x in m.sinks.iter_mut() {
                        x.current = x.name == s.name;
                    }
                    let _ = self.ch.net_tx.send(net::Req::SetSink(s.name));
                }
            }
            Hit::Clip(i) => {
                if i < m.clip.len() {
                    let (text, _) = m.clip.remove(i);
                    if !sys::copy(&text) {
                        if let Some(d) = gdk::Display::default() {
                            d.clipboard().set_text(&text);
                        }
                    }
                    m.clip.insert(0, (text, Instant::now()));
                    m.clip_copied = Some(Instant::now());
                }
            }
        }
        drop(m);
        if collapse {
            self.set_expanded(false);
        }
        self.sync_password();
        self.area.queue_draw();
    }

    /// Screen-edge glow (Wayland layer-shell): a click-through overlay over
    /// the whole screen, shown while the island is open.
    fn update_edge(&self, t: f64) {
        if t < 0.01 {
            if let Some((w, _)) = self.edge.borrow().as_ref() {
                if w.is_visible() {
                    w.set_visible(false);
                }
            }
            self.model.borrow_mut().edge_window = self.edge.borrow().is_some() && self.model.borrow().cfg.border_scope == "screen";
            return;
        }
        if self.edge.borrow().is_none() {
            let Some(app) = self.window.application() else { return };
            let w = gtk::Window::new();
            w.set_application(Some(&app));
            w.set_decorated(false);
            w.set_widget_name("notch-edge");
            if self.layer_shell {
                w.init_layer_shell();
                w.set_layer(Layer::Overlay);
                for e in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                    w.set_anchor(e, true);
                }
                w.set_exclusive_zone(-1);
                w.set_keyboard_mode(KeyboardMode::None);
                w.set_namespace(Some("notch-edge"));
            } else {
                // X11: a screen-sized dock window, made click-through
                let (sw, sh) = gdk::Display::default()
                    .and_then(|d| d.monitors().item(0))
                    .and_then(|m| m.downcast::<gdk::Monitor>().ok())
                    .map(|m| (m.geometry().width(), m.geometry().height()))
                    .unwrap_or((1920, 1080));
                w.set_default_size(sw, sh);
                w.set_resizable(false);
                w.connect_realize(|w| x11_overlay(w, true));
            }
            let area = gtk::DrawingArea::new();
            w.set_child(Some(&area));
            w.connect_map(|w| {
                // click-through everywhere
                if let Some(s) = w.surface() {
                    s.set_input_region(Some(&cairo::Region::create()));
                }
            });
            *self.edge.borrow_mut() = Some((w, area));
        }
        let edge = self.edge.borrow();
        let Some((w, area)) = edge.as_ref() else { return };
        self.model.borrow_mut().edge_window = true;
        if !w.is_visible() {
            w.set_visible(true);
        }
        let a = ((t - 0.3) / 0.6).clamp(0.0, 1.0);
        let m = self.model.borrow();
        let cfg_style = (m.cfg.border_style.clone(), m.cfg.border_color.clone(), m.art.as_ref().map(|a| a.accent));
        drop(m);
        let started = self.model.borrow().started;
        area.set_draw_func(move |_, cr, w, h| {
            let now = started.elapsed().as_secs_f64();
            draw::edge_ring(cr, w as f64, h as f64, a, now, |angle, now| {
                draw::glow_color_for(&cfg_style.0, &cfg_style.1, cfg_style.2, angle, now)
            });
        });
        area.queue_draw();
    }

    /// Volume / brightness slider moved (throttled while dragging).
    fn slider(&self, h: Hit, last: bool) {
        let mut m = self.model.borrow_mut();
        match h {
            Hit::Volume(v) => m.controls.volume = Some((v, false)),
            Hit::Brightness(v) => m.controls.brightness = Some(v),
            _ => return,
        }
        drop(m);
        if last || self.last_slider.borrow().elapsed().as_secs_f64() > 0.06 {
            *self.last_slider.borrow_mut() = Instant::now();
            let _ = self.ch.net_tx.send(match h {
                Hit::Volume(v) => net::Req::SetVolume(v),
                Hit::Brightness(v) => net::Req::SetBrightness(v),
                _ => return,
            });
        }
        self.area.queue_draw();
    }

    fn join_with_password(&self) {
        let pw = self.password.text().to_string();
        let mut m = self.model.borrow_mut();
        let Some(ssid) = m.password_for.take() else { return };
        if pw.is_empty() {
            m.password_for = Some(ssid);
            return;
        }
        m.joining = Some(ssid.clone());
        let _ = self.ch.net_tx.send(net::Req::Join { ssid, password: Some(pw) });
        drop(m);
        self.password.set_text("");
        self.sync_password();
        self.area.queue_draw();
    }

    /// Show / place / hide the password field; it needs keyboard focus, so
    /// the layer surface takes keyboard input only while it's up.
    fn sync_password(&self) {
        let rects = {
            let m = self.model.borrow();
            if m.expanded && m.page == Page::Controls { draw::password_rects(&m) } else { None }
        };
        match rects {
            Some(((x, y, w, h), _)) => {
                self.password.set_margin_start(x as i32);
                self.password.set_margin_top(y as i32);
                self.password.set_size_request(w as i32, h as i32);
                if !self.password.is_visible() {
                    if self.layer_shell {
                        self.window.set_keyboard_mode(KeyboardMode::OnDemand);
                    }
                    self.password.set_visible(true);
                    self.password.grab_focus();
                }
            }
            None if self.password.is_visible() => {
                self.password.set_visible(false);
                if self.layer_shell {
                    self.window.set_keyboard_mode(KeyboardMode::None);
                }
            }
            None => {}
        }
    }

    /// Right-click: Settings / Quit.
    fn quit_menu(&self, x: f64, y: f64) {
        let pop = gtk::Popover::new();
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let settings = gtk::Button::with_label("ตั้งค่า Notch…");
        settings.set_has_frame(false);
        let quit = gtk::Button::with_label("ออกจาก Notch");
        quit.set_has_frame(false);
        let (win, p2) = (self.window.clone(), pop.clone());
        settings.connect_clicked(move |_| {
            p2.popdown();
            if let Some(app) = win.application() {
                crate::settings::show(&app);
            }
        });
        let win = self.window.clone();
        quit.connect_clicked(move |_| {
            if let Some(app) = win.application() {
                app.quit();
            }
        });
        col.append(&settings);
        col.append(&quit);
        pop.set_child(Some(&col));
        pop.set_parent(&self.area);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.popup();
    }

    fn add_clip(&self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let mut m = self.model.borrow_mut();
        // our own copy-back lands here too: just move it to the top
        m.clip.retain(|(t, _)| *t != text);
        let text: String = text.chars().take(20_000).collect();
        m.clip.insert(0, (text, Instant::now()));
        m.clip.truncate(7);
    }

    fn banner(&self, m: &mut Model, b: Banner) {
        if m.expanded {
            return;
        }
        let now = Instant::now();
        m.banner = Some(match m.banner {
            // same HUD already up: just update the value (keeps its age)
            Some((old, at)) if old.is_level() && b.is_level() && std::mem::discriminant(&old) == std::mem::discriminant(&b) => {
                let _ = at;
                (b, now)
            }
            _ => (b, now),
        });
    }

    fn tick(&self) {
        let mut changed = false;
        {
            let mut m = self.model.borrow_mut();
            while let Ok(msg) = self.ch.mpris_rx.try_recv() {
                changed = true;
                match msg {
                    mpris::Msg::Track(t) => {
                        if t.as_ref().is_some_and(|t| t.playing) {
                            m.last_active = Instant::now();
                        }
                        m.track = t;
                        m.track_at = Instant::now();
                    }
                    mpris::Msg::Art(bytes) => m.art = bytes.as_deref().and_then(decode_art),
                }
            }
            while let Ok(s) = self.ch.status_rx.try_recv() {
                m.sessions = if m.cfg.claude_status { s } else { vec![] };
                changed = true;
            }
            while let Ok(r) = self.ch.net_rx.try_recv() {
                changed = true;
                match r {
                    net::Resp::Controls(c) => {
                        // don't fight a slider that's being dragged
                        let (v, b) = (m.controls.volume, m.controls.brightness);
                        m.controls = c;
                        if m.dragging.is_some() || self.last_slider.borrow().elapsed().as_secs_f64() < 1.0 {
                            m.controls.volume = v;
                            m.controls.brightness = b;
                        }
                    }
                    net::Resp::Nets(n) => {
                        m.nets = n;
                        m.scanning = false;
                    }
                    net::Resp::Joined { ssid, ok } => {
                        m.joining = None;
                        if !ok {
                            m.join_failed = Some(ssid.clone());
                            if m.nets.iter().any(|n| n.ssid == ssid && n.secure) {
                                m.password_for = Some(ssid);
                            }
                        }
                    }
                    net::Resp::BtDevs(d) => m.bt_devs = d,
                    net::Resp::BtDone => m.bt_busy = None,
                    net::Resp::Sinks(s) => m.sinks = s,
                }
            }
            // settings changed (settings window or config.json)?
            if self.last_poll.borrow().elapsed().as_secs_f64() > 1.0 {
                *self.last_poll.borrow_mut() = Instant::now();
                if config::reload_if_changed() {
                    m.cfg = config::get();
                    if !m.cfg.claude_status {
                        m.sessions.clear();
                    }
                    let pages = m.pages();
                    if !pages.contains(&m.page) {
                        m.page = pages[0];
                    }
                    changed = true;
                }
                // keep the controls / speakers fresh while they're on screen
                let secs = m.started.elapsed().as_secs();
                if m.expanded && secs % 4 == 0 {
                    match m.page {
                        Page::Controls => {
                            let _ = self.ch.net_tx.send(net::Req::Refresh);
                        }
                        Page::Music => {
                            let _ = self.ch.net_tx.send(net::Req::ListSinks);
                        }
                        _ => {}
                    }
                }
            }
            while let Ok(l) = self.ch.limits_rx.try_recv() {
                m.limits = Some(l);
                changed = true;
            }
            while let Ok(ev) = self.ch.sys_rx.try_recv() {
                changed = true;
                match ev {
                    sys::Event::Volume(v, muted, d) => {
                        m.controls.volume = Some((v, muted));
                        if m.cfg.banner_levels && m.dragging.is_none() && self.last_slider.borrow().elapsed().as_secs_f64() > 1.0 {
                            self.banner(&mut m, Banner::Volume(v, muted, d));
                        }
                    }
                    sys::Event::Brightness(v, d) => {
                        m.controls.brightness = Some(v);
                        if m.cfg.banner_levels && m.dragging.is_none() && self.last_slider.borrow().elapsed().as_secs_f64() > 1.0 {
                            self.banner(&mut m, Banner::Brightness(v, d));
                        }
                    }
                    sys::Event::Battery(p, charging) => {
                        let was = self.on_ac.borrow_mut().replace(charging);
                        if charging && was == Some(false) && m.cfg.banner_charging {
                            self.banner(&mut m, Banner::Charging(p));
                            *self.low_warned.borrow_mut() = 100;
                        }
                        let mut lw = self.low_warned.borrow_mut();
                        let low = m.cfg.banner_low_battery;
                        if !charging && p <= 10 && *lw > 10 {
                            *lw = 10;
                            if low {
                                self.banner(&mut m, Banner::LowBattery(p));
                            }
                        } else if !charging && p <= 20 && *lw > 20 {
                            *lw = 20;
                            if low {
                                self.banner(&mut m, Banner::LowBattery(p));
                            }
                        }
                        m.battery = Some((p, charging));
                    }
                    sys::Event::Copied(text) => {
                        drop(m);
                        self.add_clip(text);
                        m = self.model.borrow_mut();
                    }
                }
            }
            // timer finished?
            if let Some(t) = m.timer.filter(|t| t.running()) {
                if t.remaining() <= 0.0 {
                    m.timer = None;
                    m.timer_done = Some(Instant::now());
                    self.banner(&mut m, Banner::TimerDone((t.total / 60.0).round() as u32));
                    let _ = std::process::Command::new("canberra-gtk-play").args(["-i", "complete"]).spawn();
                    changed = true;
                }
            }
            if m.track.as_ref().is_some_and(|t| t.playing) {
                m.last_active = Instant::now();
            }
        }

        // no catcher (X11 without a compositor): leaving the open island
        // collapses it after a moment instead
        let left = *self.left_at.borrow();
        if self.catcher.borrow().is_none() && self.model.borrow().expanded && left.is_some_and(|t| t.elapsed().as_secs_f64() > 1.0) {
            self.set_expanded(false);
            *self.left_at.borrow_mut() = None;
        }

        // ---- animation ----
        let now = Instant::now();
        let dt = (now - *self.last_frame.borrow()).as_secs_f64().min(0.1);
        *self.last_frame.borrow_mut() = now;
        let mut animating = false;
        {
            let mut m = self.model.borrow_mut();
            let k = 1.0 - (-dt * 11.0).exp();
            let target = if m.expanded { 1.0 } else { 0.0 };
            if (m.t - target).abs() > 0.001 {
                m.t += (target - m.t) * k;
                animating = true;
            } else {
                m.t = target;
            }
            let ht = if m.hover && !m.expanded { 1.0 } else { 0.0 };
            if (m.hover_t - ht).abs() > 0.001 {
                m.hover_t += (ht - m.hover_t) * (1.0 - (-dt * 14.0).exp());
                animating = true;
            } else {
                m.hover_t = ht;
            }
            if m.banner.is_some_and(|(b, at)| at.elapsed().as_secs_f64() > b.secs()) {
                m.banner = None;
                changed = true;
            }
        }

        // ---- input region follows the island ----
        let r = draw::input_rect(&self.model.borrow());
        if r != *self.input_rect.borrow() {
            if let Some(surface) = self.window.surface() {
                let region = cairo::Region::create_rectangle(&cairo::RectangleInt::new(r.0, r.1, r.2, r.3));
                surface.set_input_region(Some(&region));
                if !surface.display().is_composited() {
                    x11_shape(r);
                }
                *self.input_rect.borrow_mut() = r;
            }
        }

        // ---- redraw: every frame while animating, else as often as the
        // visible content changes ----
        let m = self.model.borrow();
        let glow = m.expanded && m.cfg.rgb_border;
        let live = glow
            || m.track.as_ref().is_some_and(|t| t.playing) && (m.wings() || (m.expanded && m.page == Page::Music))
            || m.claude_busy()
            || m.timer.is_some()
            || m.banner.is_some()
            || m.clip_copied.is_some_and(|t| t.elapsed().as_secs_f64() < 1.6);
        let since = self.last_draw.borrow().elapsed().as_secs_f64();
        let period = if glow {
            0.033
        } else if m.wings() && !m.expanded && m.track.as_ref().is_some_and(|t| t.playing) {
            0.05
        } else {
            0.125
        };
        // screen edges need see-through windows: layer-shell, or a compositing X11
        let edge_ok = self.layer_shell || gdk::Display::default().is_some_and(|d| d.is_composited());
        let edge_t = if m.cfg.rgb_border && m.cfg.border_scope == "screen" && edge_ok { m.t } else { 0.0 };
        drop(m);
        self.update_edge(edge_t);
        if animating || changed || (live && since >= period) {
            *self.last_draw.borrow_mut() = Instant::now();
            self.area.queue_draw();
        }
    }
}
