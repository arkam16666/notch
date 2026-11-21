//! Real plan usage (% of the 5-hour and weekly limits), the same numbers as
//! Claude Code's `/usage`.
//!
//! Reads Claude Code's own OAuth login from the macOS Keychain
//! ("Claude Code-credentials"; macOS asks the user the first time) and asks
//! api.anthropic.com for the usage. The token is only ever sent to
//! api.anthropic.com (via curl's stdin config, so it never shows up in the
//! process list) and is never logged or written anywhere.
//!
//! This endpoint isn't a documented public API — if it changes, the weekly
//! bar just shows an error instead of a number.

use std::ffi::c_void;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use chrono::{DateTime, Local};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSData, NSDictionary, NSNumber, NSString};

#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    /// 0..100
    pub pct: f64,
    pub resets_at: Option<DateTime<Local>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Limits {
    pub five_hour: Option<Limit>,
    pub week: Option<Limit>,
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: *const c_void;
    static kSecClassGenericPassword: *const c_void;
    static kSecAttrService: *const c_void;
    static kSecReturnData: *const c_void;
    static kSecMatchLimit: *const c_void;
    static kSecMatchLimitOne: *const c_void;
    fn SecItemCopyMatching(query: *const c_void, result: *mut *const c_void) -> i32;
}

fn cf<'a>(p: *const c_void) -> &'a AnyObject {
    unsafe { &*(p as *const AnyObject) }
}

/// Claude Code's OAuth access token (Keychain, or ~/.claude/.credentials.json).
fn access_token() -> Result<String, String> {
    let json: Vec<u8> = unsafe {
        let keys: [&NSString; 4] = [
            &*(kSecClass as *const NSString),
            &*(kSecAttrService as *const NSString),
            &*(kSecReturnData as *const NSString),
            &*(kSecMatchLimit as *const NSString),
        ];
        let service = NSString::from_str("Claude Code-credentials");
        let yes = NSNumber::new_bool(true);
        let vals: [&AnyObject; 4] = [cf(kSecClassGenericPassword), &service, &yes, cf(kSecMatchLimitOne)];
        let query = NSDictionary::from_slices(&keys, &vals);
        let mut out: *const c_void = std::ptr::null();
        let status = SecItemCopyMatching(Retained::as_ptr(&query).cast(), &mut out);
        if status == 0 && !out.is_null() {
            let data: Retained<NSData> = Retained::from_raw(out as *mut NSData).ok_or("keychain")?;
            data.to_vec()
        } else {
            // some setups keep it in a file instead
            let path = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".claude/.credentials.json"));
            match path.and_then(|p| std::fs::read(p).ok()) {
                Some(b) => b,
                None if status == -128 => return Err("ไม่ได้อนุญาตให้อ่าน login ของ Claude Code".into()),
                None => return Err("ไม่พบ login ของ Claude Code (ลอง login ใน Claude Code)".into()),
            }
        }
    };
    let v: serde_json::Value = serde_json::from_slice(&json).map_err(|_| "อ่าน login ไม่ได้".to_string())?;
    let o = &v["claudeAiOauth"];
    if let Some(exp) = o["expiresAt"].as_i64() {
        if exp < chrono::Utc::now().timestamp_millis() {
            return Err("login หมดอายุ — เปิด Claude Code สักครั้งเพื่อรีเฟรช".into());
        }
    }
    o["accessToken"].as_str().map(str::to_string).ok_or_else(|| "ไม่พบ token ใน login".into())
}

fn parse_limit(v: &serde_json::Value) -> Option<Limit> {
    let pct = v.get("utilization")?.as_f64()?;
    let resets_at = v
        .get("resets_at")
        .and_then(|r| r.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Local));
    Some(Limit { pct, resets_at })
}

/// The token is read from the Keychain once and kept in memory, so macOS
/// asks for permission at most once per launch (and not at all after
/// "Always Allow"). It's re-read only when the server rejects it (expired).
static TOKEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn cached_token() -> Result<String, String> {
    let mut t = TOKEN.lock().unwrap();
    if let Some(tok) = t.as_ref() {
        return Ok(tok.clone());
    }
    let tok = access_token()?;
    *t = Some(tok.clone());
    Ok(tok)
}

pub fn fetch() -> Result<Limits, String> {
    match fetch_with(&cached_token()?) {
        Err(e) if e == EXPIRED => {
            // Claude Code has probably refreshed its login since → re-read once
            *TOKEN.lock().unwrap() = None;
            fetch_with(&cached_token()?)
        }
        r => r,
    }
}

const EXPIRED: &str = "login ใช้ไม่ได้ — เปิด Claude Code สักครั้งเพื่อรีเฟรช";

fn fetch_with(token: &str) -> Result<Limits, String> {
    let mut child = Command::new("/usr/bin/curl")
        .args(["-s", "-w", "\n%{http_code}", "-K", "-", "https://api.anthropic.com/api/oauth/usage"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    {
        let mut stdin = child.stdin.take().ok_or("curl")?;
        let cfg = format!(
            "header = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Accept: application/json\"\nmax-time = 10\n"
        );
        stdin.write_all(cfg.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", "0"));
    match code.trim() {
        "200" => {}
        "401" | "403" => return Err(EXPIRED.into()),
        "429" => return Err("ถูกจำกัดการเรียก รอสักครู่".into()),
        c => return Err(format!("เซิร์ฟเวอร์ตอบ {c}")),
    }
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| "รูปแบบข้อมูลเปลี่ยน".to_string())?;
    let limits = Limits {
        five_hour: v.get("five_hour").and_then(parse_limit),
        week: v.get("seven_day").and_then(parse_limit),
    };
    if limits.week.is_none() && limits.five_hour.is_none() {
        return Err("รูปแบบข้อมูลเปลี่ยน".into());
    }
    Ok(limits)
}

/// Background loop: fetch every 5 minutes, or right away when poked
/// (e.g. the Claude page was opened).
pub fn run(tx: Sender<Result<Limits, String>>, poke: Receiver<()>) {
    loop {
        let r = fetch();
        match &r {
            Ok(l) => eprintln!(
                "notch: claude usage ok (week {:?}%, 5h {:?}%)",
                l.week.as_ref().map(|x| x.pct),
                l.five_hour.as_ref().map(|x| x.pct)
            ),
            Err(e) => eprintln!("notch: claude usage error: {e}"),
        }
        if tx.send(r).is_err() {
            return;
        }
        match poke.recv_timeout(Duration::from_secs(300)) {
            Ok(()) => {
                // collapse a burst of pokes; don't hammer the API
                std::thread::sleep(Duration::from_secs(1));
                while poke.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
