//! "Ask Claude" from the notch: runs the installed Claude Code in print mode
//! (`claude -p … --output-format json`), so no API key is needed — it uses
//! the same login/plan as Claude Code. Follow-ups continue the same session
//! via `--resume <session_id>`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

pub struct Question {
    pub text: String,
    /// session to continue (None = new conversation)
    pub session: Option<String>,
}

pub struct Answer {
    pub text: String,
    pub session: Option<String>,
}

/// Where the `claude` CLI lives (apps launched from Finder don't get the
/// shell's PATH, so look in the usual install spots).
fn claude_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let candidates = [
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".npm-global/bin/claude"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn ask(q: &Question) -> Result<Answer, String> {
    let claude = claude_path().ok_or("ไม่พบ Claude Code (คำสั่ง claude)")?;
    // a quiet working directory of its own, so it doesn't poke at a project
    let cwd = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?).join(".claude/notch/ask");
    std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(claude);
    cmd.current_dir(&cwd)
        .arg("-p")
        .arg(&q.text)
        .args(["--output-format", "json"])
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin");
    if let Some(s) = &q.session {
        cmd.args(["--resume", s]);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|_| {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("Claude ไม่ตอบ");
        first.chars().take(200).collect::<String>()
    })?;
    let text = v["result"].as_str().unwrap_or("").trim().to_string();
    if v["is_error"].as_bool() == Some(true) && text.is_empty() {
        return Err(v["subtype"].as_str().unwrap_or("error").to_string());
    }
    Ok(Answer { text, session: v["session_id"].as_str().map(str::to_string) })
}

/// Background loop: one question at a time.
pub fn run(rx: Receiver<Question>, tx: Sender<Result<Answer, String>>) {
    while let Ok(q) = rx.recv() {
        if tx.send(ask(&q)).is_err() {
            return;
        }
    }
}
