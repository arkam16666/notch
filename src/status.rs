//! Live Claude Code status ("thinking…", running a tool, waiting for you, done).
//!
//! Claude Code hooks call `notch --hook <Event>` with the hook JSON on stdin;
//! [`handle_hook`] turns that into a tiny per-session state file under
//! `~/.claude/notch/sessions/`. The running notch app polls that directory
//! ([`run`]) and shows a spinner while any session is busy.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Activity {
    Thinking,
    Tool(String),
    Waiting,
    Done,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub project: String,
    pub activity: Activity,
    /// Unix time the current turn started (prompt submitted).
    pub since: f64,
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude/notch/sessions"))
}

/// Entry point for `notch --hook <Event>`. Never prints, never fails: a hook
/// must not get in Claude Code's way.
pub fn handle_hook(event: &str) {
    let _ = try_handle_hook(event);
}

fn try_handle_hook(event: &str) -> Option<()> {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut input).ok()?;
    let v: Value = serde_json::from_str(&input).ok()?;
    let sid = v.get("session_id")?.as_str()?;
    // session ids are uuids; refuse anything that could escape the directory
    if sid.is_empty() || !sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let dir = dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{sid}.json"));

    if event == "SessionEnd" {
        let _ = std::fs::remove_file(&path);
        return Some(());
    }

    let prev: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let prev_state = prev.get("state").and_then(|s| s.as_str()).unwrap_or("idle");
    let busy = matches!(prev_state, "thinking" | "tool" | "waiting");
    let t = now();
    let mut since = prev.get("since").and_then(|s| s.as_f64()).unwrap_or(t);
    let tool_name = v.get("tool_name").and_then(|s| s.as_str()).unwrap_or("");

    let (state, tool) = match event {
        "UserPromptSubmit" => {
            since = t;
            ("thinking", "")
        }
        "PreToolUse" if tool_name == "AskUserQuestion" || tool_name == "ExitPlanMode" => {
            ("waiting", "")
        }
        "PreToolUse" => ("tool", tool_name),
        "PostToolUse" | "PostToolUseFailure" | "SubagentStop" => ("thinking", ""),
        // Notifications also fire while idle ("waiting for your input"); only
        // treat them as "needs you" in the middle of a turn.
        "Notification" | "PermissionRequest" if busy => ("waiting", ""),
        "Stop" => ("done", ""),
        _ => return Some(()),
    };
    if !busy && matches!(state, "tool" | "thinking") && event != "UserPromptSubmit" {
        since = t; // e.g. the session started mid-turn before hooks existed
    }

    let data = json!({
        "state": state,
        "tool": tool,
        "since": since,
        "updated": t,
        "cwd": v.get("cwd").and_then(|s| s.as_str()).unwrap_or(""),
        "transcript": v.get("transcript_path").and_then(|s| s.as_str()).unwrap_or(""),
    });
    // write atomically so the app never reads half a file
    let tmp = dir.join(format!(".{sid}.tmp"));
    std::fs::write(&tmp, data.to_string()).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(())
}

/// Was the last thing in the transcript the user pressing Esc? (Claude Code
/// doesn't fire `Stop` for interrupts.)
fn interrupted(transcript: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(transcript) else { return false };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(4096)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    text.trim_end()
        .lines()
        .last()
        .is_some_and(|l| l.contains("Request interrupted by user"))
}

/// Current sessions worth showing (busy ones, plus "done" for a few seconds).
pub fn current() -> Vec<Session> {
    let Some(dir) = dir() else { return vec![] };
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let t = now();
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(v) = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        let state = v["state"].as_str().unwrap_or("");
        let updated = v["updated"].as_f64().unwrap_or(0.0);
        let age = t - updated;
        let activity = match state {
            "thinking" => Activity::Thinking,
            "tool" => Activity::Tool(v["tool"].as_str().unwrap_or("").to_string()),
            "waiting" => Activity::Waiting,
            "done" if age < 6.0 => Activity::Done,
            _ => continue,
        };
        // stale (crashed / killed session) or interrupted with Esc
        if activity != Activity::Done
            && (age > 30.0 * 60.0 || interrupted(v["transcript"].as_str().unwrap_or("")))
        {
            continue;
        }
        let cwd = v["cwd"].as_str().unwrap_or("");
        let project = cwd.rsplit('/').find(|s| !s.is_empty()).unwrap_or("Claude").to_string();
        out.push(Session { project, activity, since: v["since"].as_f64().unwrap_or(updated) });
    }
    // most recently started first
    out.sort_by(|a, b| b.since.total_cmp(&a.since));
    out
}

/// Background loop: poll the state directory and send changes.
pub fn run(tx: Sender<Vec<Session>>) {
    let mut last: Option<Vec<Session>> = None;
    loop {
        let s = current();
        if last.as_ref() != Some(&s) {
            if tx.send(s.clone()).is_err() {
                return;
            }
            last = Some(s);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

impl Session {
    /// Working (spinner), as opposed to waiting for the user or done.
    pub fn busy(&self) -> bool {
        matches!(self.activity, Activity::Thinking | Activity::Tool(_))
    }
}

/// Unix time now (for elapsed-time labels).
pub fn unix_now() -> f64 {
    now()
}
