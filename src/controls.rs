//! Quick system controls for the notch (a mini Control Center):
//! Wi-Fi, Bluetooth, dark mode, battery, volume and display brightness.
//!
//! - Wi-Fi: `networksetup`
//! - Bluetooth: IOBluetooth's `IOBluetoothPreference{Get,Set}ControllerPowerState`
//!   (private but long-standing; what `blueutil` uses)
//! - Brightness: DisplayServices (private, built-in display)
//! - Volume: AppleScript standard additions
//! - Dark mode: System Events (asks for Automation permission once)
//! - Battery: `pmset -g batt`

use std::ffi::{c_char, c_int, c_void, CString};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Controls {
    pub wifi: bool,
    pub wifi_connected: bool,
    pub bluetooth: Option<bool>,
    pub dark: bool,
    /// (percent, charging)
    pub battery: Option<(u8, bool)>,
    pub volume: f64,
    pub muted: bool,
    pub brightness: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Wifi(bool),
    Bluetooth(bool),
    Dark(bool),
    Volume(f64),
    Mute(bool),
}

// ---------------------------------------------------------------- dynamic libs

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
}

fn sym(lib: &str, name: &str) -> Option<usize> {
    let lib = CString::new(lib).ok()?;
    let name = CString::new(name).ok()?;
    unsafe {
        let h = dlopen(lib.as_ptr(), 2 /* RTLD_NOW */);
        if h.is_null() {
            return None;
        }
        let p = dlsym(h, name.as_ptr());
        (!p.is_null()).then_some(p as usize)
    }
}

struct Fns {
    get_brightness: Option<extern "C" fn(u32, *mut f32) -> i32>,
    set_brightness: Option<extern "C" fn(u32, f32) -> i32>,
    get_bt: Option<extern "C" fn() -> i32>,
    set_bt: Option<extern "C" fn(i32)>,
}

fn fns() -> &'static Fns {
    static F: OnceLock<Fns> = OnceLock::new();
    F.get_or_init(|| {
        const DS: &str = "/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices";
        const BT: &str = "/System/Library/Frameworks/IOBluetooth.framework/IOBluetooth";
        unsafe {
            Fns {
                get_brightness: sym(DS, "DisplayServicesGetBrightness").map(|p| std::mem::transmute(p)),
                set_brightness: sym(DS, "DisplayServicesSetBrightness").map(|p| std::mem::transmute(p)),
                get_bt: sym(BT, "IOBluetoothPreferenceGetControllerPowerState").map(|p| std::mem::transmute(p)),
                set_bt: sym(BT, "IOBluetoothPreferenceSetControllerPowerState").map(|p| std::mem::transmute(p)),
            }
        }
    })
}

pub fn brightness() -> Option<f64> {
    let f = fns().get_brightness?;
    let mut v = 0f32;
    (f(unsafe { CGMainDisplayID() }, &mut v) == 0).then_some(v as f64)
}

/// Fast enough to call on every drag event, so the main thread calls it directly.
pub fn set_brightness(v: f64) {
    if let Some(f) = fns().set_brightness {
        f(unsafe { CGMainDisplayID() }, v.clamp(0.0, 1.0) as f32);
    }
}

// ------------------------------------------------------------------- helpers

pub fn battery() -> Option<(u8, bool)> {
    run("/usr/bin/pmset", &["-g", "batt"]).as_deref().and_then(parse_battery)
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn wifi_device() -> &'static str {
    static D: OnceLock<String> = OnceLock::new();
    D.get_or_init(|| {
        run("/usr/sbin/networksetup", &["-listallhardwareports"])
            .and_then(|s| {
                let mut lines = s.lines();
                while let Some(l) = lines.next() {
                    if l.contains("Wi-Fi") || l.contains("AirPort") {
                        return lines.next()?.strip_prefix("Device: ").map(|d| d.trim().to_string());
                    }
                }
                None
            })
            .unwrap_or_else(|| "en0".into())
    })
}

pub fn parse_battery(s: &str) -> Option<(u8, bool)> {
    let line = s.lines().find(|l| l.contains("InternalBattery"))?;
    let pct = line.split('\t').nth(1)?.split('%').next()?.trim().parse().ok()?;
    let charging = line.contains("; charging") || line.contains("; charged") || line.contains("AC attached");
    Some((pct, charging))
}

pub fn read() -> Controls {
    let dev = wifi_device();
    let wifi = run("/usr/sbin/networksetup", &["-getairportpower", dev]).is_some_and(|s| s.trim().ends_with("On"));
    let wifi_connected = wifi && run("/usr/sbin/ipconfig", &["getifaddr", dev]).is_some_and(|s| !s.trim().is_empty());
    let (volume, muted) = run("/usr/bin/osascript", &["-e", "get volume settings"])
        .map(|s| {
            let get = |k: &str| {
                s.split(", ")
                    .find_map(|kv| kv.trim().strip_prefix(k))
                    .map(|v| v.trim().to_string())
            };
            let vol = get("output volume:").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) / 100.0;
            (vol, get("output muted:").as_deref() == Some("true"))
        })
        .unwrap_or((0.0, false));
    Controls {
        wifi,
        wifi_connected,
        bluetooth: fns().get_bt.map(|f| f() != 0),
        dark: run("/usr/bin/defaults", &["read", "-g", "AppleInterfaceStyle"]).is_some_and(|s| s.trim() == "Dark"),
        battery: run("/usr/bin/pmset", &["-g", "batt"]).as_deref().and_then(parse_battery),
        volume,
        muted,
        brightness: brightness(),
    }
}

fn apply(a: Action) {
    match a {
        Action::Wifi(on) => {
            run("/usr/sbin/networksetup", &["-setairportpower", wifi_device(), if on { "on" } else { "off" }]);
        }
        Action::Bluetooth(on) => {
            if let Some(f) = fns().set_bt {
                f(on as i32);
            }
        }
        Action::Dark(on) => {
            run(
                "/usr/bin/osascript",
                &["-e", &format!(
                    "tell application \"System Events\" to tell appearance preferences to set dark mode to {on}"
                )],
            );
        }
        Action::Volume(v) => {
            let n = (v.clamp(0.0, 1.0) * 100.0).round() as i32;
            run("/usr/bin/osascript", &["-e", &format!("set volume output volume {n} without output muted")]);
        }
        Action::Mute(m) => {
            run("/usr/bin/osascript", &["-e", &format!("set volume output muted {m}")]);
        }
    }
}

/// Background loop: apply actions (coalescing rapid volume changes while
/// dragging) and re-read the state every 2 s.
pub fn run_loop(tx: Sender<Controls>, actions: Receiver<Action>) {
    let mut last: Option<Controls> = None;
    loop {
        let c = read();
        if last.as_ref() != Some(&c) {
            if tx.send(c.clone()).is_err() {
                return;
            }
            last = Some(c);
        }
        match actions.recv_timeout(Duration::from_secs(2)) {
            Ok(first) => {
                let mut pending = vec![first];
                while let Ok(a) = actions.try_recv() {
                    pending.push(a);
                }
                // only the latest volume matters
                let last_vol = pending.iter().rposition(|a| matches!(a, Action::Volume(_)));
                for (i, a) in pending.into_iter().enumerate() {
                    if matches!(a, Action::Volume(_)) && Some(i) != last_vol {
                        continue;
                    }
                    apply(a);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn battery() {
        let s = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=27328611)\t100%; discharging; 8:39 remaining present: true\n";
        assert_eq!(super::parse_battery(s), Some((100, false)));
        let s = " -InternalBattery-0 (id=1)\t57%; charging; 1:02 remaining present: true";
        assert_eq!(super::parse_battery(s), Some((57, true)));
    }
}
