//! Screen mirroring helpers.
//!
//! macOS has no public API to *start* AirPlay screen mirroring, so the notch
//! lists the AirPlay receivers on the network (Bonjour `_airplay._tcp`, via
//! the system `dns-sd` tool) and hands the actual connection to Apple's own
//! Displays settings. Whether the screen is being mirrored right now comes
//! from CoreGraphics.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayIsInMirrorSet(display: u32) -> i32;
}

/// Is the screen mirrored to another display (AirPlay or cable) right now?
pub fn mirroring() -> bool {
    unsafe { CGDisplayIsInMirrorSet(CGMainDisplayID()) != 0 }
}

fn computer_name() -> String {
    Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Browse for AirPlay receivers for `secs` seconds, then send the names
/// (this Mac itself excluded, sorted, de-duplicated).
pub fn scan(secs: f64, tx: Sender<Vec<String>>) {
    std::thread::spawn(move || {
        let me = computer_name();
        let mut names: Vec<String> = Vec::new();
        if let Ok(mut child) = Command::new("/usr/bin/dns-sd")
            .args(["-B", "_airplay._tcp", "local."])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            let out = child.stdout.take();
            // reader thread; we kill dns-sd after `secs`
            let (ltx, lrx) = std::sync::mpsc::channel::<String>();
            if let Some(out) = out {
                std::thread::spawn(move || {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if ltx.send(line).is_err() {
                            break;
                        }
                    }
                });
            }
            let end = Instant::now() + Duration::from_secs_f64(secs);
            while let Some(left) = end.checked_duration_since(Instant::now()) {
                match lrx.recv_timeout(left) {
                    Ok(line) => {
                        if let Some((add, name)) = parse_line(&line) {
                            if add {
                                if name != me && !names.contains(&name) {
                                    names.push(name);
                                }
                            } else {
                                names.retain(|n| *n != name);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        names.sort_by_key(|n| n.to_lowercase());
        let _ = tx.send(names);
    });
}

/// " 8:20:22.306  Add  3  1 local.  _airplay._tcp.  Living Room TV"
/// → (true, "Living Room TV")
fn parse_line(line: &str) -> Option<(bool, String)> {
    let idx = line.find("_airplay._tcp.")?;
    let name = line[idx + "_airplay._tcp.".len()..].trim().to_string();
    let add = line.split_whitespace().nth(1)? == "Add";
    if name.is_empty() { None } else { Some((add, name)) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses() {
        let l = " 8:20:22.306  Add        3   1 local.               _airplay._tcp.       Living Room TV";
        assert_eq!(super::parse_line(l), Some((true, "Living Room TV".into())));
        let l = " 8:20:22.306  Rmv        0   1 local.               _airplay._tcp.       Living Room TV";
        assert_eq!(super::parse_line(l), Some((false, "Living Room TV".into())));
        assert_eq!(super::parse_line("Timestamp     A/R    Flags  if Domain"), None);
    }
}
