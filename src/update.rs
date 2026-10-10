//! Update check against GitHub Releases: release parsing and version
//! comparison. Pure code; the download/install side lives in `win::updater`.

use serde_json::Value;

pub const REPO: &str = "rodrigohpalmeirim/claude-taskbar-usage";
pub const EXE_ASSET: &str = "ClaudeTaskbarUsage.exe";
pub const SHA_ASSET: &str = "ClaudeTaskbarUsage.exe.sha256";

/// The latest published release, if it carries the files we need.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// "0.2.0" (without the leading "v").
    pub version: String,
    /// Release notes page.
    pub page_url: String,
    pub exe_url: String,
    pub sha_url: String,
}

pub fn latest_release_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

/// Parses `GET /repos/{owner}/{repo}/releases/latest`.
pub fn parse_release(body: &[u8]) -> Option<Release> {
    let v: Value = serde_json::from_slice(body).ok()?;
    if v.get("draft").and_then(Value::as_bool) == Some(true) || v.get("prerelease").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let tag = v.get("tag_name")?.as_str()?;
    let version = tag.strip_prefix('v').unwrap_or(tag).to_string();
    parse_version(&version)?;
    let asset = |name: &str| -> Option<String> {
        v.get("assets")?
            .as_array()?
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(name))?
            .get("browser_download_url")?
            .as_str()
            .map(str::to_string)
    };
    Some(Release {
        version,
        page_url: v.get("html_url").and_then(Value::as_str).unwrap_or_default().to_string(),
        exe_url: asset(EXE_ASSET)?,
        sha_url: asset(SHA_ASSET)?,
    })
}

/// "1.2.3" -> (1, 2, 3). Pre-release suffixes are rejected.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split('.').map(|p| p.parse::<u32>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(v)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

/// Reads a `sha256sum` line ("<64 hex chars>  filename").
pub fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    let hex = text.split_whitespace().next()?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = r#"{
      "tag_name": "v0.2.0", "draft": false, "prerelease": false,
      "html_url": "https://github.com/rodrigohpalmeirim/claude-taskbar-usage/releases/tag/v0.2.0",
      "assets": [
        {"name": "ClaudeTaskbarUsage.exe", "browser_download_url": "https://github.com/x/releases/download/v0.2.0/ClaudeTaskbarUsage.exe"},
        {"name": "ClaudeTaskbarUsage.exe.sha256", "browser_download_url": "https://github.com/x/releases/download/v0.2.0/ClaudeTaskbarUsage.exe.sha256"}
      ]
    }"#;

    #[test]
    fn parses_release() {
        let r = parse_release(RELEASE.as_bytes()).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.exe_url.ends_with("/ClaudeTaskbarUsage.exe"));
        assert!(r.sha_url.ends_with(".sha256"));
    }

    #[test]
    fn needs_both_assets_and_a_final_release() {
        let no_sha = RELEASE.replace("ClaudeTaskbarUsage.exe.sha256", "other.txt");
        assert!(parse_release(no_sha.as_bytes()).is_none());
        let pre = RELEASE.replace(r#""prerelease": false"#, r#""prerelease": true"#);
        assert!(parse_release(pre.as_bytes()).is_none());
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2.0-beta", "0.1.0"));
    }

    #[test]
    fn reads_sha256sum_output() {
        let line = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08  ClaudeTaskbarUsage.exe\n";
        let h = parse_sha256(line).unwrap();
        assert_eq!(h[0], 0x9f);
        assert_eq!(h[31], 0x08);
        assert!(parse_sha256("nothex").is_none());
    }
}
