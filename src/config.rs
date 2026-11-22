//! User settings, shared by the notch and the separate "Notch Settings" app.
//!
//! Stored as JSON in `~/.claude/notch/config.json`. The settings app writes it
//! on every change; the notch re-reads it when the file's mtime changes
//! (checked about once a second), so changes apply immediately.

use std::path::PathBuf;
use std::sync::RwLock;
use std::time::SystemTime;

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    // tabs
    pub tab_music: bool,
    pub tab_claude: bool,
    pub tab_shelf: bool,
    pub tab_clipboard: bool,
    pub tab_timer: bool,
    pub tab_weather: bool,
    pub tab_camera: bool,
    pub tab_controls: bool,
    // collapsed island
    pub hover_bounce: bool,
    pub claude_status: bool,
    pub camera_dot: bool,
    pub waveform: bool,
    /// seconds before the wings fold away after music stops (0 = never fold)
    pub idle_secs: u32,
    // banners
    pub banner_charging: bool,
    pub banner_low_battery: bool,
    pub banner_levels: bool,
    // expanded island
    pub sysbar: bool,
    pub battery: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tab_music: true,
            tab_claude: true,
            tab_shelf: true,
            tab_clipboard: true,
            tab_timer: true,
            tab_weather: true,
            tab_camera: true,
            tab_controls: true,
            hover_bounce: true,
            claude_status: true,
            camera_dot: true,
            waveform: true,
            idle_secs: 5,
            banner_charging: true,
            banner_low_battery: true,
            banner_levels: true,
            sysbar: true,
            battery: true,
        }
    }
}

/// Every boolean setting, by JSON key (the settings app builds its
/// checkboxes from this, so the two never drift apart).
pub const BOOL_KEYS: [&str; 17] = [
    "tab_music",
    "tab_claude",
    "tab_shelf",
    "tab_clipboard",
    "tab_timer",
    "tab_weather",
    "tab_camera",
    "tab_controls",
    "hover_bounce",
    "claude_status",
    "camera_dot",
    "waveform",
    "banner_charging",
    "banner_low_battery",
    "banner_levels",
    "sysbar",
    "battery",
];

impl Config {
    pub fn get_bool(&self, key: &str) -> bool {
        self.clone().field_mut(key).is_none_or(|f| *f)
    }

    pub fn set_bool(&mut self, key: &str, v: bool) {
        if let Some(f) = self.field_mut(key) {
            *f = v;
        }
    }

    fn field_mut(&mut self, key: &str) -> Option<&mut bool> {
        Some(match key {
            "tab_music" => &mut self.tab_music,
            "tab_claude" => &mut self.tab_claude,
            "tab_shelf" => &mut self.tab_shelf,
            "tab_clipboard" => &mut self.tab_clipboard,
            "tab_timer" => &mut self.tab_timer,
            "tab_weather" => &mut self.tab_weather,
            "tab_camera" => &mut self.tab_camera,
            "tab_controls" => &mut self.tab_controls,
            "hover_bounce" => &mut self.hover_bounce,
            "claude_status" => &mut self.claude_status,
            "camera_dot" => &mut self.camera_dot,
            "waveform" => &mut self.waveform,
            "banner_charging" => &mut self.banner_charging,
            "banner_low_battery" => &mut self.banner_low_battery,
            "banner_levels" => &mut self.banner_levels,
            "sysbar" => &mut self.sysbar,
            "battery" => &mut self.battery,
            _ => return None,
        })
    }

    fn from_json(v: &Value) -> Config {
        let mut c = Config::default();
        for k in BOOL_KEYS {
            if let Some(b) = v.get(k).and_then(Value::as_bool) {
                c.set_bool(k, b);
            }
        }
        if let Some(n) = v.get("idle_secs").and_then(Value::as_u64) {
            c.idle_secs = n.min(3600) as u32;
        }
        c
    }

    fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        for k in BOOL_KEYS {
            m.insert(k.into(), json!(self.get_bool(k)));
        }
        m.insert("idle_secs".into(), json!(self.idle_secs));
        Value::Object(m)
    }
}

pub fn path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude/notch/config.json"))
}

pub fn load() -> Config {
    path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|v| Config::from_json(&v))
        .unwrap_or_default()
}

pub fn save(c: &Config) -> std::io::Result<()> {
    let p = path().ok_or_else(|| std::io::Error::other("no HOME"))?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&c.to_json()).unwrap_or_default())?;
    std::fs::rename(tmp, p)
}

static CURRENT: RwLock<Option<(Config, Option<SystemTime>)>> = RwLock::new(None);

fn mtime() -> Option<SystemTime> {
    path().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

/// The current settings (cached; call `reload_if_changed` to pick up edits).
pub fn get() -> Config {
    if let Some((c, _)) = CURRENT.read().unwrap().as_ref() {
        return c.clone();
    }
    let c = load();
    *CURRENT.write().unwrap() = Some((c.clone(), mtime()));
    c
}

/// Re-read the file if it changed on disk. Returns true if any setting changed.
pub fn reload_if_changed() -> bool {
    let m = mtime();
    let old = CURRENT.read().unwrap().clone();
    if let Some((_, seen)) = &old {
        if *seen == m {
            return false;
        }
    }
    let c = load();
    let changed = old.as_ref().is_none_or(|(o, _)| *o != c);
    *CURRENT.write().unwrap() = Some((c, m));
    changed
}
