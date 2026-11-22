//! Everything the island shows, in one plain struct. The UI fills it from
//! the background threads; `draw.rs` paints it; `--snapshot` fills it with
//! demo data instead.

use std::time::Instant;

use gtk::cairo;

use crate::claude_api::Limits;
use crate::mpris::Track;
use crate::net::{BtDev, Controls, Net, Sink};
use crate::status::Session;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Music,
    Claude,
    Timer,
    Clipboard,
    Controls,
}

pub const PAGES: [Page; 5] = [Page::Music, Page::Claude, Page::Timer, Page::Clipboard, Page::Controls];

/// In-island list opened from the controls page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    Wifi,
    Bluetooth,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Banner {
    /// volume 0..1, muted, direction
    Volume(f64, bool, i8),
    Brightness(f64, i8),
    Charging(u8),
    LowBattery(u8),
    /// countdown finished (minutes)
    TimerDone(u32),
}

impl Banner {
    pub fn secs(self) -> f64 {
        match self {
            Banner::Volume(..) | Banner::Brightness(..) => 1.8,
            Banner::Charging(_) => 3.2,
            Banner::LowBattery(_) => 4.0,
            Banner::TimerDone(_) => 6.0,
        }
    }
    pub fn is_level(self) -> bool {
        matches!(self, Banner::Volume(..) | Banner::Brightness(..))
    }
}

pub const TIMER_PRESETS: [u32; 5] = [1, 5, 10, 25, 45];

#[derive(Clone, Copy, Debug)]
pub struct Timer {
    pub total: f64,
    /// seconds left when last paused / started
    pub left: f64,
    pub started: Option<Instant>,
}

impl Timer {
    pub fn new(secs: f64) -> Self {
        Timer { total: secs, left: secs, started: Some(Instant::now()) }
    }
    pub fn remaining(&self) -> f64 {
        match self.started {
            Some(s) => (self.left - s.elapsed().as_secs_f64()).max(0.0),
            None => self.left,
        }
    }
    pub fn running(&self) -> bool {
        self.started.is_some()
    }
}

/// Things that can be clicked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Island,
    Tab(Page),
    Prev,
    Play,
    Next,
    /// progress bar, 0..1
    Seek(f64),
    TimerPreset(usize),
    TimerToggle,
    TimerReset,
    TimerPlus,
    Clip(usize),
    ClipClear,
    // controls page
    TileWifi,
    TileBt,
    TileDark,
    /// the "›" on a tile: open its list
    More(Detail),
    /// slider value 0..1
    Volume(f64),
    Brightness(f64),
    Back,
    DetailSwitch,
    NetRow(usize),
    BtRow(usize),
    Join,
    // music page speaker picker
    OutBtn,
    OutRow(usize),
    OutBack,
}

pub struct Art {
    /// square, 256 px
    pub surface: cairo::ImageSurface,
    pub accent: (f64, f64, f64),
}

pub struct Model {
    pub expanded: bool,
    /// 0 = collapsed … 1 = open (animated)
    pub t: f64,
    pub hover: bool,
    /// 0…1 hover bulge (animated)
    pub hover_t: f64,
    pub hover_hit: Option<Hit>,
    pub page: Page,

    pub track: Option<Track>,
    /// when `track.position` was read (to extrapolate)
    pub track_at: Instant,
    pub art: Option<Art>,
    /// keep the wings out a few seconds after music stops
    pub last_active: Instant,

    pub sessions: Vec<Session>,
    pub limits: Option<Result<Limits, String>>,

    pub timer: Option<Timer>,
    pub timer_last: u32,
    pub timer_done: Option<Instant>,

    pub clip: Vec<(String, Instant)>,
    pub clip_copied: Option<Instant>,

    pub banner: Option<(Banner, Instant)>,
    pub battery: Option<(u8, bool)>,

    pub controls: Controls,
    pub detail: Option<Detail>,
    pub nets: Vec<Net>,
    pub scanning: bool,
    pub joining: Option<String>,
    pub join_failed: Option<String>,
    /// network the password field is open for
    pub password_for: Option<String>,
    pub bt_devs: Vec<BtDev>,
    pub bt_busy: Option<String>,
    pub sinks: Vec<Sink>,
    pub out_open: bool,
    /// slider being dragged (so polling doesn't fight the finger)
    pub dragging: Option<Hit>,

    /// the screen-edge glow window exists (Wayland layer-shell only)
    pub edge_window: bool,
    /// settings (shared with the macOS app's config.json)
    pub cfg: crate::config::Config,

    pub started: Instant,
}

impl Default for Model {
    fn default() -> Self {
        let now = Instant::now();
        Model {
            expanded: false,
            t: 0.0,
            hover: false,
            hover_t: 0.0,
            hover_hit: None,
            page: Page::Music,
            track: None,
            track_at: now,
            art: None,
            last_active: now - std::time::Duration::from_secs(3600),
            sessions: vec![],
            limits: None,
            timer: None,
            timer_last: 25,
            timer_done: None,
            clip: vec![],
            clip_copied: None,
            banner: None,
            battery: None,
            controls: Controls::default(),
            detail: None,
            nets: vec![],
            scanning: false,
            joining: None,
            join_failed: None,
            password_for: None,
            bt_devs: vec![],
            bt_busy: None,
            sinks: vec![],
            out_open: false,
            dragging: None,
            edge_window: false,
            cfg: crate::config::Config::default(),
            started: now,
        }
    }
}

impl Model {
    /// Song position now (extrapolated between polls).
    pub fn position(&self) -> f64 {
        match &self.track {
            Some(t) => {
                let p = t.position + if t.playing { self.track_at.elapsed().as_secs_f64() } else { 0.0 };
                if t.length > 0.0 { p.min(t.length) } else { p }
            }
            None => 0.0,
        }
    }

    pub fn claude_busy(&self) -> bool {
        self.sessions.iter().any(|s| s.busy())
    }

    /// Something to show on the collapsed island's sides.
    pub fn wings(&self) -> bool {
        let music = self.track.as_ref().is_some_and(|t| t.playing) || self.last_active.elapsed().as_secs_f64() < 5.0;
        (self.track.is_some() && music) || !self.sessions.is_empty() || self.timer.is_some()
    }

    /// Tabs switched on in the settings (never empty).
    pub fn pages(&self) -> Vec<Page> {
        let c = &self.cfg;
        let v: Vec<Page> = PAGES
            .iter()
            .copied()
            .filter(|p| match p {
                Page::Music => c.tab_music,
                Page::Claude => c.tab_claude,
                Page::Timer => c.tab_timer,
                Page::Clipboard => c.tab_clipboard,
                Page::Controls => c.tab_controls,
            })
            .collect();
        if v.is_empty() { vec![Page::Music] } else { v }
    }

    pub fn banner_live(&self) -> Option<Banner> {
        self.banner.filter(|(b, at)| at.elapsed().as_secs_f64() < b.secs()).map(|(b, _)| b)
    }
}
