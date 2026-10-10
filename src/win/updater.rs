//! Daily update check against GitHub Releases and one-click self-update:
//! download the new exe, verify its SHA-256 against the release's checksum
//! file, swap it in next to the running one and restart.
//!
//! Network access here goes only to api.github.com and GitHub's release
//! download hosts, and only while `check_updates` is on.

use super::http;
use crate::update::{self, Release};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn user_agent() -> String {
    format!("ClaudeTaskbarUsage/{VERSION}")
}

/// The newest release if it is newer than this build. Err on network or API
/// errors (worth retrying sooner than a day).
pub fn check() -> Result<Option<Release>, ()> {
    let headers = "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n";
    let resp = http::get(&update::latest_release_url(), headers, &user_agent(), 1 << 20)?;
    match resp.status {
        200 => Ok(update::parse_release(&resp.body).filter(|r| update::is_newer(&r.version, VERSION))),
        404 => Ok(None), // no release published yet
        _ => Err(()),
    }
}

fn download(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let resp = http::get(url, "", &user_agent(), max_bytes).map_err(|()| "download failed".to_string())?;
    if resp.status != 200 {
        return Err(format!("download failed (HTTP {})", resp.status));
    }
    Ok(resp.body)
}

fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    // SAFETY: one-shot hash with the SHA-256 pseudo-handle (Windows 10+);
    // input and output buffers have the stated lengths.
    let status = unsafe {
        BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr().cast_mut(), data.len() as u32, out.as_mut_ptr(), 32)
    };
    (status == 0).then_some(out)
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

/// Downloads, verifies and swaps in the new exe. Returns the path to start.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let expected = download(&release.sha_url, 64 * 1024)
        .ok()
        .and_then(|b| update::parse_sha256(&String::from_utf8_lossy(&b)))
        .ok_or("couldn't read the release checksum")?;
    let data = download(&release.exe_url, 32 << 20)?;
    if !data.starts_with(b"MZ") || sha256(&data) != Some(expected) {
        return Err("the download didn't match the release checksum".into());
    }

    // A running exe can be renamed but not overwritten: move it aside first.
    let new = sibling(&exe, ".new");
    let old = sibling(&exe, ".old");
    std::fs::write(&new, &data).map_err(|e| format!("can't write next to the app ({e})"))?;
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(&exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("can't replace the app ({e})"));
    }
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("can't replace the app ({e})"));
    }
    Ok(exe)
}

/// Removes the previous exe left behind by an update (it can't delete
/// itself while running, so the new version does it).
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(sibling(&exe, ".old"));
    }
}
