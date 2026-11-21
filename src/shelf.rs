//! File shelf: files dropped on the notch to park them for later. Only the
//! paths are kept (in ~/.claude/notch/shelf.json); the files stay put.

use std::path::PathBuf;

fn store() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude/notch/shelf.json"))
}

/// Saved shelf, minus files that no longer exist.
pub fn load() -> Vec<PathBuf> {
    let Some(p) = store() else { return vec![] };
    let Ok(b) = std::fs::read(p) else { return vec![] };
    let list: Vec<String> = serde_json::from_slice(&b).unwrap_or_default();
    list.into_iter().map(PathBuf::from).filter(|p| p.exists()).collect()
}

pub fn save(items: &[PathBuf]) {
    let Some(p) = store() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let list: Vec<String> = items.iter().map(|p| p.to_string_lossy().to_string()).collect();
    if let Ok(json) = serde_json::to_vec(&list) {
        let _ = std::fs::write(p, json);
    }
}
