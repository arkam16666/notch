//! `notch --snapshot DIR`: render every page with demo data to PNGs —
//! checks the look without a display (e.g. in a container).

use std::time::{Duration, Instant};

use gtk::cairo;

use crate::claude_api::{Limit, Limits};
use crate::draw::{self, WIN_H, WIN_W};
use crate::model::{Art, Banner, Model, Page, Timer};
use crate::mpris::Track;
use crate::status::{Activity, Session};

/// Made-up artwork (gradient + rings), so no real cover art ends up in
/// screenshots.
fn demo_art() -> Art {
    let s = cairo::ImageSurface::create(cairo::Format::ARgb32, 256, 256).unwrap();
    {
        let cr = cairo::Context::new(&s).unwrap();
        let g = cairo::LinearGradient::new(0.0, 0.0, 256.0, 256.0);
        g.add_color_stop_rgb(0.0, 0.55, 0.2, 0.95);
        g.add_color_stop_rgb(0.6, 0.35, 0.2, 0.75);
        g.add_color_stop_rgb(1.0, 0.95, 0.35, 0.45);
        cr.set_source(&g).unwrap();
        cr.paint().unwrap();
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.25);
        cr.set_line_width(5.0);
        for r in [40.0, 70.0, 100.0] {
            cr.arc(128.0, 128.0, r, 0.0, 2.0 * std::f64::consts::PI);
            cr.stroke().unwrap();
        }
    }
    let accent = crate::ui::accent_of(&s).unwrap_or((0.8, 0.5, 1.0));
    Art { surface: s, accent }
}

fn net(ssid: &str, signal: u8, secure: bool, current: bool, known: bool) -> crate::net::Net {
    crate::net::Net { ssid: ssid.into(), signal, secure, current, known }
}
fn bt(name: &str, connected: bool, audio: bool) -> crate::net::BtDev {
    crate::net::BtDev { mac: name.into(), name: name.into(), connected, audio }
}
fn sink(d: &str, current: bool, bluetooth: bool, headphones: bool) -> crate::net::Sink {
    crate::net::Sink { name: d.into(), description: d.into(), current, bluetooth, headphones }
}

fn demo() -> Model {
    let ago = |s: u64| Instant::now() - Duration::from_secs(s);
    let now = chrono::Local::now();
    Model {
        track: Some(Track {
            bus: "demo".into(),
            app: "Spotify".into(),
            title: "Midnight Drive".into(),
            artist: "The Placeholders".into(),
            art_url: None,
            track_id: None,
            length: 213.0,
            position: 85.0,
            playing: true,
            can_seek: true,
        }),
        art: Some(demo_art()),
        last_active: Instant::now(),
        limits: Some(Ok(Limits {
            five_hour: Some(Limit { pct: 34.0, resets_at: Some(now + chrono::Duration::minutes(150)) }),
            week: Some(Limit { pct: 61.0, resets_at: Some(now + chrono::Duration::days(3)) }),
        })),
        sessions: vec![Session { project: "notch".into(), activity: Activity::Tool("Bash".into()), since: 0.0 }],
        clip: vec![
            ("cargo build --release && cargo deb".into(), ago(5)),
            ("https://github.com/arkam16666/notch".into(), ago(200)),
            ("ประชุมทีมวันพฤหัส 14:00 ห้อง 3B".into(), ago(720)),
            ("sudo dnf install ./notch-0.1.0-1.x86_64.rpm".into(), ago(1500)),
            ("Midnight Drive — The Placeholders".into(), ago(2900)),
        ],
        battery: Some((86, false)),
        controls: crate::net::Controls {
            wifi: Some(true),
            wifi_ssid: Some("Home 5G".into()),
            bluetooth: Some(true),
            bt_device: Some("WH-1000XM5".into()),
            dark: Some(true),
            volume: Some((0.45, false)),
            brightness: Some(0.6),
        },
        nets: vec![
            net("Home 5G", 90, true, true, true),
            net("Office", 70, true, false, true),
            net("Cafe Free WiFi", 55, false, false, false),
            net("TrueMove H", 40, true, false, false),
            net("Neighbor", 20, true, false, false),
        ],
        bt_devs: vec![
            bt("WH-1000XM5", true, true),
            bt("MX Master 3S", true, false),
            bt("JBL Flip 6", false, true),
        ],
        sinks: vec![
            sink("Built-in Audio Analog Stereo", false, false, false),
            sink("WH-1000XM5", true, true, false),
            sink("HDMI / DisplayPort", false, false, false),
        ],
        started: ago(3),
        ..Default::default()
    }
}

pub fn render(dir: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let shots: Vec<(&str, Box<dyn Fn(&mut Model)>)> = vec![
        ("collapsed", Box::new(|_| {})),
        ("idle", Box::new(|m| {
            m.track = None;
            m.sessions.clear();
            m.last_active = Instant::now() - Duration::from_secs(60);
        })),
        ("music", Box::new(|m| m.expanded = true)),
        ("music-hover", Box::new(|m| {
            m.expanded = true;
            m.hover_hit = Some(crate::model::Hit::Play);
        })),
        ("claude", Box::new(|m| {
            m.expanded = true;
            m.page = Page::Claude;
        })),
        ("timer", Box::new(|m| {
            m.expanded = true;
            m.page = Page::Timer;
            m.timer = Some(Timer { total: 1500.0, left: 1500.0, started: Some(Instant::now() - Duration::from_secs(378)) });
        })),
        ("timer-collapsed", Box::new(|m| {
            m.timer = Some(Timer { total: 1500.0, left: 1500.0, started: Some(Instant::now() - Duration::from_secs(378)) });
        })),
        ("clipboard", Box::new(|m| {
            m.expanded = true;
            m.page = Page::Clipboard;
            m.hover_hit = Some(crate::model::Hit::Clip(1));
        })),
        ("controls", Box::new(|m| {
            m.expanded = true;
            m.page = Page::Controls;
        })),
        ("wifi", Box::new(|m| {
            m.expanded = true;
            m.page = Page::Controls;
            m.detail = Some(crate::model::Detail::Wifi);
            m.password_for = Some("TrueMove H".into());
        })),
        ("bluetooth", Box::new(|m| {
            m.expanded = true;
            m.page = Page::Controls;
            m.detail = Some(crate::model::Detail::Bluetooth);
        })),
        ("speakers", Box::new(|m| {
            m.expanded = true;
            m.out_open = true;
        })),
        ("glow-artwork", Box::new(|m| {
            m.expanded = true;
            m.cfg.border_style = "artwork".into();
        })),
        ("volume", Box::new(|m| m.banner = Some((Banner::Volume(0.62, false, -1), Instant::now())))),
        ("charging", Box::new(|m| m.banner = Some((Banner::Charging(86), Instant::now())))),
    ];
    for (name, setup) in shots {
        let mut m = demo();
        setup(&mut m);
        m.t = if m.expanded { 1.0 } else { 0.0 };
        // crop to the island (+ a little margin), at 2× for crisp PNGs
        let (x, w, h, _) = draw::island(&m);
        let (cx, cw, ch) = ((x - 14.0).max(0.0), (w + 28.0).min(WIN_W), (h + 6.0).min(WIN_H));
        let scale = 2.0;
        let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, (cw * scale) as i32, (ch * scale) as i32).unwrap();
        {
            let cr = cairo::Context::new(&surf).unwrap();
            // a mid-grey "desktop" so the black island is visible
            cr.set_source_rgb(0.16, 0.17, 0.2);
            cr.paint().unwrap();
            cr.scale(scale, scale);
            cr.translate(-cx, 0.0);
            // draw() clears its own area first; paint on a group so the
            // background stays
            cr.push_group();
            draw::draw(&cr, &m);
            cr.pop_group_to_source().unwrap();
            cr.paint().unwrap();
        }
        let path = format!("{dir}/{name}.png");
        let mut f = std::fs::File::create(&path).unwrap();
        surf.write_to_png(&mut f).unwrap();
        println!("wrote {path}");
    }
}
