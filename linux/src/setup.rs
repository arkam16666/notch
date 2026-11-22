//! `notch --setup-claude`: point Claude Code's hooks at this binary so the
//! island can show what Claude is doing. Edits ~/.claude/settings.json in
//! place (keeps everything else; updates an older Notch hook if present).

use serde_json::{json, Value};

const EVENTS: [&str; 7] = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure", "Notification", "Stop", "SessionEnd"];
const TOOL_EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "PostToolUseFailure"];

pub fn claude_hooks() {
    let Some(home) = std::env::var_os("HOME") else { return };
    let dir = std::path::PathBuf::from(home).join(".claude");
    if !dir.is_dir() {
        eprintln!("ไม่พบ Claude Code (~/.claude) — ติดตั้ง Claude Code ก่อน");
        std::process::exit(1);
    }
    let path = dir.join("settings.json");
    let mut cfg: Value = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    let bin = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "notch".into());
    let Some(obj) = cfg.as_object_mut() else { return };
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return };
    for ev in EVENTS {
        let cmd = format!("\"{bin}\" --hook {ev} 2>/dev/null || true");
        let groups = hooks.entry(ev).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else { continue };
        let mut found = false;
        for g in groups.iter_mut() {
            if let Some(hs) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                for h in hs {
                    let c = h.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    if c.contains("--hook") && c.to_lowercase().contains("notch") {
                        h["command"] = json!(cmd);
                        found = true;
                    }
                }
            }
        }
        if !found {
            let mut g = json!({ "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }] });
            if TOOL_EVENTS.contains(&ev) {
                g["matcher"] = json!("*");
            }
            groups.push(g);
        }
    }
    let tmp = path.with_extension("json.notch-tmp");
    let text = serde_json::to_string_pretty(&cfg).unwrap_or_default() + "\n";
    if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).is_ok() {
        println!("ตั้ง hooks ของ Claude Code แล้ว → {}", path.display());
    }
}
