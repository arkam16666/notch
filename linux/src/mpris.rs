//! Now playing, from any MPRIS player on the session bus (Spotify, Firefox /
//! Chrome tabs incl. YouTube, VLC, mpv, Rhythmbox, Elisa…). Plain D-Bus: no
//! browser extension, no permissions.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// bus name of the player
    pub bus: String,
    /// "Spotify", "Firefox", …
    pub app: String,
    pub title: String,
    pub artist: String,
    pub art_url: Option<String>,
    pub track_id: Option<String>,
    /// seconds (0 = unknown / live)
    pub length: f64,
    pub position: f64,
    pub playing: bool,
    pub can_seek: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Cmd {
    PlayPause,
    Next,
    Previous,
    /// absolute position in seconds
    Seek(f64),
}

pub enum Msg {
    Track(Option<Track>),
    /// image bytes (PNG / JPEG / …) of the artwork, None = no artwork
    Art(Option<Vec<u8>>),
}

fn player(conn: &Connection, bus: &str) -> Option<Proxy<'static>> {
    Proxy::new(conn, bus.to_string(), PATH, PLAYER).ok()
}

fn string(v: &OwnedValue) -> Option<String> {
    match &**v {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

fn strings(v: &OwnedValue) -> Vec<String> {
    match &**v {
        Value::Array(a) => a
            .iter()
            .filter_map(|x| match x {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        Value::Str(s) => vec![s.to_string()],
        _ => vec![],
    }
}

fn number(v: &OwnedValue) -> Option<f64> {
    match &**v {
        Value::I64(n) => Some(*n as f64),
        Value::U64(n) => Some(*n as f64),
        Value::I32(n) => Some(*n as f64),
        Value::U32(n) => Some(*n as f64),
        Value::F64(n) => Some(*n),
        _ => None,
    }
}

/// "org.mpris.MediaPlayer2.firefox.instance_1_23" → "Firefox"
fn app_name(conn: &Connection, bus: &str) -> String {
    if let Ok(p) = Proxy::new(conn, bus.to_string(), PATH, "org.mpris.MediaPlayer2") {
        if let Ok(id) = p.get_property::<String>("Identity") {
            if !id.is_empty() {
                return id;
            }
        }
    }
    let short = bus.trim_start_matches(PREFIX).split('.').next().unwrap_or("");
    let mut c = short.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn read(conn: &Connection, bus: &str) -> Option<Track> {
    let p = player(conn, bus)?;
    let status: String = p.get_property("PlaybackStatus").ok()?;
    let meta: HashMap<String, OwnedValue> = p.get_property("Metadata").ok()?;
    let title = meta.get("xesam:title").and_then(string).unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    let artist = meta.get("xesam:artist").map(strings).unwrap_or_default().join(", ");
    let length = meta.get("mpris:length").and_then(number).map(|us| us / 1e6).unwrap_or(0.0);
    let position = p.get_property::<i64>("Position").map(|us| us as f64 / 1e6).unwrap_or(0.0);
    Some(Track {
        bus: bus.to_string(),
        app: app_name(conn, bus),
        title,
        artist,
        art_url: meta.get("mpris:artUrl").and_then(string).filter(|s| !s.is_empty()),
        track_id: meta.get("mpris:trackid").and_then(string),
        length,
        position,
        playing: status == "Playing",
        can_seek: p.get_property::<bool>("CanSeek").unwrap_or(false),
    })
}

/// The player to show: the one playing, else the last one that was, else any
/// paused one.
fn current(conn: &Connection, last_bus: Option<&str>) -> Option<Track> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let names = dbus.list_names().ok()?;
    let tracks: Vec<Track> = names
        .iter()
        .map(|n| n.as_str())
        .filter(|n| n.starts_with(PREFIX))
        .filter_map(|n| read(conn, n))
        .collect();
    tracks
        .iter()
        .find(|t| t.playing)
        .or_else(|| tracks.iter().find(|t| Some(t.bus.as_str()) == last_bus))
        .or_else(|| tracks.first())
        .cloned()
}

fn run_cmd(conn: &Connection, t: &Track, cmd: Cmd) {
    let Some(p) = player(conn, &t.bus) else { return };
    let _ = match cmd {
        Cmd::PlayPause => p.call_method("PlayPause", &()),
        Cmd::Next => p.call_method("Next", &()),
        Cmd::Previous => p.call_method("Previous", &()),
        Cmd::Seek(secs) => {
            let us = (secs.max(0.0) * 1e6) as i64;
            match t.track_id.as_deref().and_then(|id| ObjectPath::try_from(id).ok()) {
                Some(id) => p.call_method("SetPosition", &(id, us)),
                // no track id: relative seek from where we think we are
                None => p.call_method("Seek", &(us - (t.position * 1e6) as i64)),
            }
        }
    };
}

/// Artwork bytes: a local file (file://…) or downloaded (http(s)://…).
fn fetch_art(url: &str) -> Option<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        let path = percent_decode(path);
        return std::fs::read(path).ok();
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let out = std::process::Command::new("curl").args(["-sL", "-m", "8", url]).output().ok()?;
        return (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout);
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Background loop: poll once a second (or right after a command).
pub fn run(tx: Sender<Msg>, cmds: Receiver<Cmd>) {
    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("notch: no session D-Bus ({e}) — music disabled");
            return;
        }
    };
    let mut last: Option<Track> = None;
    let mut last_art: Option<String> = None;
    loop {
        let now = current(&conn, last.as_ref().map(|t| t.bus.as_str()));
        if now != last {
            if tx.send(Msg::Track(now.clone())).is_err() {
                return;
            }
            let art = now.as_ref().and_then(|t| t.art_url.clone());
            if art != last_art {
                let _ = tx.send(Msg::Art(art.as_deref().and_then(fetch_art)));
                last_art = art;
            }
            last = now;
        }
        match cmds.recv_timeout(Duration::from_secs(1)) {
            Ok(cmd) => {
                if let Some(t) = &last {
                    run_cmd(&conn, t, cmd);
                    // collapse a burst of seeks into the last one
                    while let Ok(c) = cmds.try_recv() {
                        run_cmd(&conn, t, c);
                    }
                }
                std::thread::sleep(Duration::from_millis(120));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
