//! Claude Taskbar Usage: Claude's 5-hour and weekly rate limits as a compact
//! widget docked into the Windows taskbar.
#![cfg_attr(windows, windows_subsystem = "windows")]
// Most of the code is only reachable from the Windows entry point.
#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod timefmt;
mod usage;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    win::app::run();
    #[cfg(not(windows))]
    eprintln!("Claude Taskbar Usage only runs on Windows.");
}
