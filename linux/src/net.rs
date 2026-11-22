//! Wi-Fi (NetworkManager's `nmcli`), Bluetooth (BlueZ's `bluetoothctl`),
//! audio outputs (`pactl`), dark mode, and setting volume / brightness.
//! Everything slow runs on a worker thread (`run`).

use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Controls {
    pub wifi: Option<bool>,
    pub wifi_ssid: Option<String>,
    pub bluetooth: Option<bool>,
    /// connected device name (+N)
    pub bt_device: Option<String>,
    pub dark: Option<bool>,
    pub volume: Option<(f64, bool)>,
    pub brightness: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub ssid: String,
    pub signal: u8,
    pub secure: bool,
    pub current: bool,
    /// a saved connection exists (no password needed)
    pub known: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BtDev {
    pub mac: String,
    pub name: String,
    pub connected: bool,
    pub audio: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sink {
    pub name: String,
    pub description: String,
    pub current: bool,
    pub bluetooth: bool,
    pub headphones: bool,
}

pub enum Req {
    Refresh,
    ScanWifi,
    SetWifi(bool),
    Join { ssid: String, password: Option<String> },
    SetBt(bool),
    ListBt,
    BtConnect { mac: String, connect: bool },
    ListSinks,
    SetSink(String),
    SetDark(bool),
    SetVolume(f64),
    SetBrightness(f64),
}

pub enum Resp {
    Controls(Controls),
    Nets(Vec<Net>),
    Joined { ssid: String, ok: bool },
    BtDevs(Vec<BtDev>),
    BtDone,
    Sinks(Vec<Sink>),
}

fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    run_timeout(cmd, args, Duration::from_secs(6))
}

/// Run a command, give up (and kill it) after `limit` — a stuck tool must
/// never freeze the island.
fn run_timeout(cmd: &str, args: &[&str], limit: Duration) -> Option<String> {
    use std::io::Read;
    let mut child = Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let end = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = reader.join().unwrap_or_default();
                return status.success().then_some(text);
            }
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("notch: {cmd} {} timed out", args.join(" "));
                return None;
            }
        }
    }
}

/// `nmcli -t` fields are ':'-separated with '\:' escapes.
fn split_terse(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut esc = false;
    for c in line.chars() {
        match (esc, c) {
            (true, _) => {
                out.last_mut().unwrap().push(c);
                esc = false;
            }
            (false, '\\') => esc = true,
            (false, ':') => out.push(String::new()),
            (false, _) => out.last_mut().unwrap().push(c),
        }
    }
    out
}

// -------------------------------------------------------------------- wifi

fn wifi_on() -> Option<bool> {
    run_cmd("nmcli", &["radio", "wifi"]).map(|s| s.trim() == "enabled")
}

fn wifi_ssid() -> Option<String> {
    // --rescan no: answer from NetworkManager's cache (a rescan takes seconds)
    let s = run_cmd("nmcli", &["-t", "-f", "ACTIVE,SSID", "dev", "wifi", "list", "--rescan", "no"])?;
    s.lines().map(split_terse).find(|f| f.first().map(String::as_str) == Some("yes")).and_then(|f| f.get(1).cloned()).filter(|s| !s.is_empty())
}

fn known_ssids() -> Vec<String> {
    run_cmd("nmcli", &["-t", "-f", "NAME,TYPE", "connection", "show"])
        .map(|s| {
            s.lines()
                .map(split_terse)
                .filter(|f| f.get(1).is_some_and(|t| t.contains("wireless")))
                .filter_map(|f| f.first().cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// Networks NetworkManager already knows about (instant).
pub fn scan_wifi() -> Vec<Net> {
    let known = known_ssids();
    let Some(s) = run_cmd("nmcli", &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list", "--rescan", "no"]) else {
        return vec![];
    };
    let mut nets: Vec<Net> = Vec::new();
    for f in s.lines().map(split_terse) {
        let (Some(inuse), Some(ssid), Some(sig), Some(sec)) = (f.first(), f.get(1), f.get(2), f.get(3)) else { continue };
        if ssid.is_empty() {
            continue;
        }
        let n = Net {
            ssid: ssid.clone(),
            signal: sig.parse().unwrap_or(0),
            secure: !sec.is_empty() && sec != "--",
            current: inuse == "*",
            known: known.contains(ssid),
        };
        match nets.iter_mut().find(|x| x.ssid == n.ssid) {
            Some(x) => {
                x.signal = x.signal.max(n.signal);
                x.current |= n.current;
            }
            None => nets.push(n),
        }
    }
    nets.sort_by(|a, b| b.current.cmp(&a.current).then(b.known.cmp(&a.known)).then(b.signal.cmp(&a.signal)));
    nets.truncate(20);
    nets
}

fn join(ssid: &str, password: Option<&str>) -> bool {
    if password.is_none() && known_ssids().iter().any(|k| k == ssid) {
        return run_cmd("nmcli", &["connection", "up", "id", ssid]).is_some();
    }
    let mut args = vec!["--wait", "30", "dev", "wifi", "connect", ssid];
    if let Some(p) = password {
        args.extend(["password", p]);
    }
    run_timeout("nmcli", &args, Duration::from_secs(35)).is_some()
}

// --------------------------------------------------------------- bluetooth

fn bt_on() -> Option<bool> {
    let s = run_cmd("bluetoothctl", &["show"])?;
    if s.trim().is_empty() {
        return None; // no adapter
    }
    Some(s.lines().any(|l| l.trim() == "Powered: yes"))
}

fn parse_devices(s: &str) -> Vec<(String, String)> {
    s.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Device ")?;
            let (mac, name) = rest.split_once(' ')?;
            Some((mac.to_string(), name.to_string()))
        })
        .collect()
}

/// Paired devices: `devices Paired` (BlueZ 5.65+) or `paired-devices` (older).
fn bt_paired() -> Vec<(String, String)> {
    let v = run_cmd("bluetoothctl", &["devices", "Paired"]).map(|s| parse_devices(&s)).unwrap_or_default();
    if !v.is_empty() {
        return v;
    }
    run_cmd("bluetoothctl", &["paired-devices"]).map(|s| parse_devices(&s)).unwrap_or_default()
}

/// (connected, audio) from `bluetoothctl info` — works with every version.
fn bt_info(mac: &str) -> (bool, bool) {
    let Some(s) = run_cmd("bluetoothctl", &["info", mac]) else { return (false, false) };
    let connected = s.lines().any(|l| l.trim() == "Connected: yes");
    let audio = s.lines().any(|l| {
        let l = l.trim();
        l.starts_with("Icon: audio") || l.contains("Audio Sink") || l.contains("0000110b")
    });
    (connected, audio)
}

#[allow(dead_code)]
fn bt_list(kind: &str) -> Vec<(String, String)> {
    run_cmd("bluetoothctl", &["devices", kind])
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let rest = l.strip_prefix("Device ")?;
                    let (mac, name) = rest.split_once(' ')?;
                    Some((mac.to_string(), name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[allow(dead_code)]
fn bt_is_audio(mac: &str) -> bool {
    run_cmd("bluetoothctl", &["info", mac]).is_some_and(|s| {
        s.lines().any(|l| {
            let l = l.trim();
            l.starts_with("Icon: audio") || l.contains("Audio Sink") || l.contains("0000110b")
        })
    })
}

pub fn bt_devices() -> Vec<BtDev> {
    let mut v: Vec<BtDev> = bt_paired()
        .into_iter()
        .map(|(mac, name)| {
            let (connected, audio) = bt_info(&mac);
            BtDev { connected, audio, mac, name }
        })
        .collect();
    v.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.audio.cmp(&a.audio)).then(a.name.cmp(&b.name)));
    v
}

fn bt_connected_label() -> Option<String> {
    let c: Vec<(String, String)> = bt_paired().into_iter().filter(|(mac, _)| bt_info(mac).0).collect();
    let first = c.first()?.1.clone();
    Some(if c.len() > 1 { format!("{first} +{}", c.len() - 1) } else { first })
}

// ------------------------------------------------------------------- audio

pub fn sinks() -> Vec<Sink> {
    let default = run_cmd("pactl", &["get-default-sink"]).unwrap_or_default().trim().to_string();
    let Some(json) = run_cmd("pactl", &["-f", "json", "list", "sinks"]) else { return vec![] };
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(&json) else { return vec![] };
    list.iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?.to_string();
            let description = s.get("description").and_then(|d| d.as_str()).unwrap_or(&name).to_string();
            let active_port = s.get("active_port").and_then(|p| p.as_str()).unwrap_or("").to_lowercase();
            Some(Sink {
                current: name == default,
                bluetooth: name.starts_with("bluez"),
                headphones: active_port.contains("headphone") || description.to_lowercase().contains("headphone"),
                name,
                description,
            })
        })
        .collect()
}

fn set_sink(name: &str) {
    if run_cmd("pactl", &["set-default-sink", name]).is_none() {
        return;
    }
    // move what's playing now as well
    if let Some(s) = run_cmd("pactl", &["list", "short", "sink-inputs"]) {
        for id in s.lines().filter_map(|l| l.split_whitespace().next()) {
            let _ = run_cmd("pactl", &["move-sink-input", id, name]);
        }
    }
}

// --------------------------------------------------------------- dark mode

/// GNOME / GTK desktops: org.gnome.desktop.interface color-scheme; KDE:
/// the colour scheme name in kdeglobals.
pub fn dark() -> Option<bool> {
    if let Some(s) = run_cmd("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"]) {
        return Some(s.contains("dark"));
    }
    let home = std::env::var_os("HOME")?;
    let kg = std::fs::read_to_string(std::path::PathBuf::from(home).join(".config/kdeglobals")).ok()?;
    kg.lines().find_map(|l| l.strip_prefix("ColorScheme=")).map(|s| s.to_lowercase().contains("dark"))
}

fn set_dark(on: bool) {
    if Command::new("plasma-apply-colorscheme").arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
        let _ = run_cmd("plasma-apply-colorscheme", &[if on { "BreezeDark" } else { "BreezeLight" }]);
    }
    let _ = run_cmd("gsettings", &["set", "org.gnome.desktop.interface", "color-scheme", if on { "prefer-dark" } else { "default" }]);
}

// -------------------------------------------------------- volume, brightness

fn set_volume(v: f64) {
    let pct = format!("{}%", (v.clamp(0.0, 1.0) * 100.0).round() as u32);
    let _ = run_cmd("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "0"]);
    let _ = run_cmd("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &pct]);
}

/// Through systemd-logind (no root needed for the active session).
fn set_brightness(v: f64) {
    let Some(dir) = std::fs::read_dir("/sys/class/backlight").ok().and_then(|mut d| d.next()).and_then(|e| e.ok()) else { return };
    let name = dir.file_name().to_string_lossy().into_owned();
    let max: f64 = std::fs::read_to_string(dir.path().join("max_brightness")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
    if max <= 0.0 {
        return;
    }
    let value = (v.clamp(0.01, 1.0) * max).round() as u32;
    if let Ok(conn) = zbus::blocking::Connection::system() {
        let _ = conn.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1/session/auto",
            Some("org.freedesktop.login1.Session"),
            "SetBrightness",
            &("backlight", name.as_str(), value),
        );
    }
}

pub fn read() -> Controls {
    let wifi = wifi_on();
    let bluetooth = bt_on();
    Controls {
        wifi,
        wifi_ssid: if wifi == Some(true) { wifi_ssid() } else { None },
        bluetooth,
        bt_device: if bluetooth == Some(true) { bt_connected_label() } else { None },
        dark: dark(),
        volume: crate::sys::volume(),
        brightness: crate::sys::brightness(),
    }
}

/// Two workers: audio / brightness / dark mode answer at once, while Wi-Fi
/// and Bluetooth (which can take many seconds) queue on their own thread.
pub fn run(rx: Receiver<Req>, tx: Sender<Resp>) {
    let (fast_tx, fast_rx) = mpsc::channel();
    let (slow_tx, slow_rx) = mpsc::channel();
    let t = tx.clone();
    std::thread::spawn(move || worker(fast_rx, t));
    std::thread::spawn(move || worker(slow_rx, tx));
    while let Ok(req) = rx.recv() {
        let fast = matches!(req, Req::ListSinks | Req::SetSink(_) | Req::SetDark(_) | Req::SetVolume(_) | Req::SetBrightness(_));
        let ok = if fast { fast_tx.send(req).is_ok() } else { slow_tx.send(req).is_ok() };
        if !ok {
            return;
        }
    }
}

fn worker(rx: Receiver<Req>, tx: Sender<Resp>) {
    while let Ok(req) = rx.recv() {
        let mut out = Vec::new();
        match req {
            Req::Refresh => {}
            Req::ScanWifi => {
                // what's cached now, then a real scan in the background and
                // the fresher list a few seconds later
                out.push(Resp::Nets(scan_wifi()));
                let tx2 = tx.clone();
                std::thread::spawn(move || {
                    let _ = run_timeout("nmcli", &["dev", "wifi", "rescan"], Duration::from_secs(10));
                    std::thread::sleep(Duration::from_secs(3));
                    let _ = tx2.send(Resp::Nets(scan_wifi()));
                });
            }
            Req::SetWifi(on) => {
                let _ = run_cmd("nmcli", &["radio", "wifi", if on { "on" } else { "off" }]);
            }
            Req::Join { ssid, password } => {
                let ok = join(&ssid, password.as_deref());
                out.push(Resp::Joined { ssid, ok });
                out.push(Resp::Nets(scan_wifi()));
            }
            Req::SetBt(on) => {
                let _ = run_cmd("bluetoothctl", &["power", if on { "on" } else { "off" }]);
            }
            Req::ListBt => out.push(Resp::BtDevs(bt_devices())),
            Req::BtConnect { mac, connect } => {
                // (no --timeout: with it bluetoothctl waits it out even after
                // connecting; ours kills it if it truly hangs)
                let _ = run_timeout("bluetoothctl", &[if connect { "connect" } else { "disconnect" }, &mac], Duration::from_secs(20));
                out.push(Resp::BtDone);
                out.push(Resp::BtDevs(bt_devices()));
                out.push(Resp::Sinks(sinks()));
            }
            Req::ListSinks => out.push(Resp::Sinks(sinks())),
            Req::SetSink(name) => {
                set_sink(&name);
                out.push(Resp::Sinks(sinks()));
            }
            Req::SetDark(on) => set_dark(on),
            Req::SetVolume(v) => set_volume(v),
            Req::SetBrightness(v) => set_brightness(v),
        }
        out.push(Resp::Controls(read()));
        for r in out {
            if tx.send(r).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_terse;

    #[test]
    fn terse_fields() {
        assert_eq!(split_terse("*:Home 5G:90:WPA2"), vec!["*", "Home 5G", "90", "WPA2"]);
        // colons inside an SSID are escaped by nmcli
        assert_eq!(split_terse(" :My\\:Net:40:"), vec![" ", "My:Net", "40", ""]);
        assert_eq!(split_terse("yes:"), vec!["yes", ""]);
    }
}

#[cfg(test)]
mod bt_tests {
    #[test]
    fn devices_lines() {
        let s = "Device AA:BB:CC:00:00:01 WH-1000XM5\nDevice AA:BB:CC:00:00:02 MX Master 3S\nnot a device";
        assert_eq!(
            super::parse_devices(s),
            vec![("AA:BB:CC:00:00:01".into(), "WH-1000XM5".into()), ("AA:BB:CC:00:00:02".into(), "MX Master 3S".into())]
        );
    }
}
