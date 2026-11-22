//! Notch for Linux: a Dynamic Island at the top centre of the desktop.
//!
//!   notch                     run it
//!   notch --settings          open the settings window
//!   notch --setup-claude      add Claude Code hooks (status spinner)
//!   notch --hook <Event>      called by those hooks
//!   notch --snapshot <dir>    render demo PNGs (no display needed)

// shared with the macOS app (plain Rust, no platform code)
#[path = "../../src/claude_api.rs"]
mod claude_api;
#[allow(dead_code)]
#[path = "../../src/config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../../src/status.rs"]
mod status;

mod draw;
mod model;
mod mpris;
mod net;
mod settings;
mod setup;
mod snapshot;
mod sys;
mod ui;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).map(|i| args.get(i + 1).cloned());
    if let Some(ev) = arg("--hook") {
        return status::handle_hook(ev.as_deref().unwrap_or(""));
    }
    if args.iter().any(|a| a == "--setup-claude") {
        return setup::claude_hooks();
    }
    if let Some(dir) = arg("--snapshot") {
        return snapshot::render(dir.as_deref().unwrap_or("."));
    }
    if args.iter().any(|a| a == "--version") {
        return println!("notch {}", env!("CARGO_PKG_VERSION"));
    }
    ui::run(args);
}
