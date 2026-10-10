//! Claude Code CLI helpers: the User-Agent mirrors the installed version, and
//! `claude update` renews an expired OAuth token (same approach as
//! usage-monitor-for-claude).

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const FALLBACK_USER_AGENT: &str = "claude-code/2.1.204";

fn claude_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for ext in ["exe", "cmd", "bat"] {
            let p = dir.join("claude").with_extension(ext);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    [home.map(|h| h.join(r".local\bin\claude.exe")), appdata.map(|a| a.join(r"npm\claude.cmd"))].into_iter().flatten().find(|p| p.is_file())
}

/// Runs the CLI hidden and returns its stdout, killing it after `timeout`.
fn run(args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(claude_path()?)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(200)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

pub fn user_agent() -> String {
    run(&["--version"], Duration::from_secs(20))
        .and_then(|out| first_version(&out).map(|v| format!("claude-code/{v}")))
        .unwrap_or_else(|| FALLBACK_USER_AGENT.into())
}

pub fn update() -> bool {
    run(&["update"], Duration::from_secs(90)).is_some()
}

/// First "1.2.3"-shaped token in `s`.
fn first_version(s: &str) -> Option<&str> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.')).find(|t| t.split('.').count() == 3 && t.split('.').all(|p| !p.is_empty()))
}
