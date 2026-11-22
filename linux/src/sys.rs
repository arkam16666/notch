//! System state for the banners: volume (PipeWire / PulseAudio via
//! `pactl`), screen brightness and battery (sysfs), and clipboard history
//! (`wl-paste --watch` on Wayland).

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

pub enum Event {
    /// volume 0..1, muted, direction (+1 / -1 / 0)
    Volume(f64, bool, i8),
    /// brightness 0..1, direction
    Brightness(f64, i8),
    /// (percent, charging) — sent every few seconds
    Battery(u8, bool),
    /// text copied somewhere
    Copied(String),
}

// ------------------------------------------------------------------ volume

fn pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// (volume 0..1, muted) of the default output.
pub fn volume() -> Option<(f64, bool)> {
    let v = pactl(&["get-sink-volume", "@DEFAULT_SINK@"])?;
    // "Volume: front-left: 39321 /  60% / -13.31 dB,   front-right: …"
    let pct = v.split('/').nth(1)?.trim().trim_end_matches('%').trim().parse::<f64>().ok()?;
    let muted = pactl(&["get-sink-mute", "@DEFAULT_SINK@"]).is_some_and(|m| m.contains("yes"));
    Some(((pct / 100.0).clamp(0.0, 1.5), muted))
}

/// Watch `pactl subscribe` for sink changes → Volume events.
fn watch_volume(tx: Sender<Event>) {
    let mut last = volume();
    let Ok(mut child) = Command::new("pactl").arg("subscribe").stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        eprintln!("notch: pactl not found — no volume banner");
        return;
    };
    let Some(out) = child.stdout.take() else { return };
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        if !(line.contains("'change'") && line.contains(" sink ")) {
            continue;
        }
        let Some(v) = volume() else { continue };
        if let Some(old) = last {
            if (v.0 - old.0).abs() > 0.004 || v.1 != old.1 {
                let dir = if v.1 && !old.1 { 0 } else if v.0 > old.0 { 1 } else if v.0 < old.0 { -1 } else { 0 };
                if tx.send(Event::Volume(v.0.min(1.0), v.1, dir)).is_err() {
                    break;
                }
            }
        }
        last = Some(v);
    }
    let _ = child.kill();
}

// --------------------------------------------------------------- brightness

fn backlight() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/backlight").ok()?.filter_map(|e| e.ok()).map(|e| e.path()).next()
}

fn read_num(p: PathBuf) -> Option<f64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

pub fn brightness() -> Option<f64> {
    let dir = backlight()?;
    let cur = read_num(dir.join("actual_brightness")).or_else(|| read_num(dir.join("brightness")))?;
    let max = read_num(dir.join("max_brightness"))?;
    (max > 0.0).then(|| (cur / max).clamp(0.0, 1.0))
}

// ------------------------------------------------------------------ battery

pub fn battery() -> Option<(u8, bool)> {
    for e in std::fs::read_dir("/sys/class/power_supply").ok()?.filter_map(|e| e.ok()) {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let pct = std::fs::read_to_string(p.join("capacity")).ok()?.trim().parse::<u8>().ok()?;
        let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
        let charging = matches!(status.trim(), "Charging" | "Full");
        return Some((pct.min(100), charging));
    }
    None
}

/// Poll brightness (5×/s; sysfs reads are cheap) and battery (every 5 s).
fn poll(tx: Sender<Event>) {
    let mut last_b = brightness();
    let mut n = 0u32;
    loop {
        if let Some(b) = brightness() {
            if let Some(old) = last_b {
                if (b - old).abs() > 0.02 {
                    let dir = if b > old { 1 } else { -1 };
                    if tx.send(Event::Brightness(b, dir)).is_err() {
                        return;
                    }
                }
            }
            last_b = Some(b);
        }
        if n % 25 == 0 {
            if let Some((p, c)) = battery() {
                if tx.send(Event::Battery(p, c)).is_err() {
                    return;
                }
            }
        }
        n = n.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------- clipboard

/// Wayland: `wl-paste --watch` runs a command on every copy. We have it
/// print the text followed by a NUL so copies can be told apart. (Needs a
/// compositor with the data-control protocol: KDE, wlroots… On X11 the GTK
/// clipboard signal is used instead, see ui.rs.)
fn watch_clipboard(tx: Sender<Event>) -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    let Ok(mut child) = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text", "--watch", "sh", "-c", "cat; printf '\\0'"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let Some(mut out) = child.stdout.take() else { return false };
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(n) = out.read(&mut chunk) {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            while let Some(i) = buf.iter().position(|b| *b == 0) {
                let text = String::from_utf8_lossy(&buf[..i]).into_owned();
                buf.drain(..=i);
                if !text.trim().is_empty() && tx.send(Event::Copied(text)).is_err() {
                    let _ = child.kill();
                    return;
                }
            }
        }
    });
    true
}

/// Put text on the clipboard: `wl-copy` on Wayland; returns false if that
/// isn't available (the UI then uses GTK's clipboard).
pub fn copy(text: &str) -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().is_ok_and(|s| s.success())
}

/// Start the watchers. Returns whether clipboard history is handled here
/// (Wayland) — if not, the UI listens to GTK's clipboard.
pub fn start(tx: Sender<Event>) -> bool {
    let t = tx.clone();
    std::thread::spawn(move || watch_volume(t));
    let t = tx.clone();
    std::thread::spawn(move || poll(t));
    watch_clipboard(tx)
}
