//! Wi-Fi network list / joining and Bluetooth device list / connecting,
//! for the in-notch detail views. Runs on a background thread because
//! scanning and connecting block for seconds.
//!
//! - Wi-Fi scan: CoreWLAN. SSIDs are only visible once the app has Location
//!   permission (macOS treats nearby network names as location data).
//! - Wi-Fi join: `networksetup -setairportnetwork` (uses the saved password
//!   from the keychain for known networks).
//! - Bluetooth: IOBluetooth paired devices, open/close baseband connection.

use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

use objc2::rc::Retained;
use objc2_core_wlan::{CWNetwork, CWSecurity, CWWiFiClient};
use objc2_foundation::NSString;
use objc2_io_bluetooth::IOBluetoothDevice;

#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub ssid: String,
    pub rssi: i64,
    pub secure: bool,
    pub known: bool,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BtDev {
    pub name: String,
    pub addr: String,
    pub connected: bool,
}

pub enum Req {
    ScanWifi,
    Join { ssid: String, password: Option<String> },
    ListBt,
    SetBt { addr: String, connect: bool },
}

pub enum Resp {
    /// Err("location") = SSIDs hidden (no Location permission)
    Wifi(Result<Vec<Net>, String>),
    Joined { ssid: String, ok: bool },
    Bt(Vec<BtDev>),
    BtDone,
}

fn scan_wifi() -> Result<Vec<Net>, String> {
    let iface = unsafe { CWWiFiClient::sharedWiFiClient().interface() }.ok_or("no Wi-Fi interface")?;
    let current = unsafe { iface.ssid() }.map(|s| s.to_string());
    let known: Vec<String> = unsafe { iface.configuration() }
        .map(|c| unsafe { c.networkProfiles() }.array().iter().filter_map(|p| unsafe { p.ssid() }).map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let found = unsafe { iface.scanForNetworksWithName_error(None) }.map_err(|e| e.localizedDescription().to_string())?;
    let nets: Vec<Retained<CWNetwork>> = found.iter().collect();
    if !nets.is_empty() && nets.iter().all(|n| unsafe { n.ssid() }.is_none()) {
        return Err("location".into());
    }
    // one entry per SSID (strongest access point)
    let mut by_ssid: HashMap<String, Net> = HashMap::new();
    for n in nets {
        let Some(ssid) = (unsafe { n.ssid() }).map(|s| s.to_string()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let rssi = unsafe { n.rssiValue() } as i64;
        let secure = !unsafe { n.supportsSecurity(CWSecurity::None) };
        let e = by_ssid.entry(ssid.clone()).or_insert(Net {
            known: known.contains(&ssid),
            current: current.as_deref() == Some(&ssid),
            ssid,
            rssi,
            secure,
        });
        e.rssi = e.rssi.max(rssi);
    }
    // the joined network may not show up in a scan
    if let Some(cur) = &current {
        by_ssid.entry(cur.clone()).or_insert(Net {
            ssid: cur.clone(),
            rssi: unsafe { iface.rssiValue() } as i64,
            secure: true,
            known: true,
            current: true,
        });
    }
    let mut v: Vec<Net> = by_ssid.into_values().collect();
    v.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then(b.known.cmp(&a.known))
            .then(b.rssi.cmp(&a.rssi))
    });
    Ok(v)
}

fn wifi_device() -> String {
    unsafe { CWWiFiClient::sharedWiFiClient().interface() }
        .and_then(|i| unsafe { i.interfaceName() })
        .map(|s| s.to_string())
        .unwrap_or_else(|| "en0".into())
}

/// `networksetup` prints nothing on success and an error line on failure.
fn join(ssid: &str, password: Option<&str>) -> bool {
    let dev = wifi_device();
    let mut cmd = Command::new("/usr/sbin/networksetup");
    cmd.args(["-setairportnetwork", &dev, ssid]);
    if let Some(p) = password {
        cmd.arg(p);
    }
    match cmd.output() {
        Ok(o) => {
            let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            o.status.success() && out.trim().is_empty()
        }
        Err(_) => false,
    }
}

fn list_bt() -> Vec<BtDev> {
    let Some(arr) = (unsafe { IOBluetoothDevice::pairedDevices() }) else { return vec![] };
    let mut v: Vec<BtDev> = arr
        .iter()
        .filter_map(|o| o.downcast::<IOBluetoothDevice>().ok())
        .map(|d| unsafe {
            BtDev {
                name: d.nameOrAddress().map(|s| s.to_string()).unwrap_or_else(|| "อุปกรณ์".into()),
                addr: d.addressString().map(|s| s.to_string()).unwrap_or_default(),
                connected: d.isConnected(),
            }
        })
        .collect();
    v.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    v
}

fn set_bt(addr: &str, connect: bool) {
    let Some(d) = (unsafe { IOBluetoothDevice::deviceWithAddressString(Some(&NSString::from_str(addr))) }) else {
        return;
    };
    unsafe {
        if connect {
            d.openConnection();
        } else {
            d.closeConnection();
        }
    }
}

pub fn run(rx: Receiver<Req>, tx: Sender<Resp>) {
    while let Ok(req) = rx.recv() {
        let resp = match req {
            Req::ScanWifi => vec![Resp::Wifi(scan_wifi())],
            Req::Join { ssid, password } => {
                let ok = join(&ssid, password.as_deref());
                vec![Resp::Joined { ssid, ok }, Resp::Wifi(scan_wifi())]
            }
            Req::ListBt => vec![Resp::Bt(list_bt())],
            Req::SetBt { addr, connect } => {
                set_bt(&addr, connect);
                vec![Resp::BtDone, Resp::Bt(list_bt())]
            }
        };
        for r in resp {
            if tx.send(r).is_err() {
                return;
            }
        }
    }
}

/// `notch --scan`: quick check from the terminal.
pub fn debug_print() {
    println!("wifi: {:#?}", scan_wifi());
    println!("bluetooth: {:#?}", list_bt());
}
