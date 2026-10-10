//! The subset of `/api/oauth/usage` the widget shows, plus its parser.

use crate::timefmt::{DAY, HOUR, days_from_civil};
use serde_json::Value;

/// One rate-limit window.
#[derive(Clone, Debug, PartialEq)]
pub struct Quota {
    pub name: String,
    pub percent: f64,
    /// None when the window has not started yet.
    pub resets_at: Option<i64>,
    /// Window length in ms.
    pub period: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    /// `five_hour`
    pub session: Option<Quota>,
    /// `seven_day`
    pub weekly: Option<Quota>,
    /// Model-scoped and other quotas, tooltip only.
    pub others: Vec<Quota>,
}

pub const PERIOD_5H: i64 = 5 * HOUR;
pub const PERIOD_7D: i64 = 7 * DAY;

/// A failed fetch, with enough detail to pick a retry strategy.
#[derive(Clone, Debug)]
pub struct FetchError {
    /// Fits in the taskbar.
    pub short: &'static str,
    /// Tooltip detail.
    pub msg: String,
    pub auth: bool,
    pub retry_after_secs: u64,
}

impl FetchError {
    pub fn new(short: &'static str, msg: impl Into<String>) -> Self {
        Self { short, msg: msg.into(), auth: false, retry_after_secs: 0 }
    }
}

/// Friendly names for known per-model / per-product weekly quotas. Unknown
/// code-named quotas are skipped: they have no public meaning yet.
const OTHER_QUOTAS: &[(&str, &str)] = &[
    ("seven_day_cowork", "Weekly Cowork"),
    ("seven_day_oauth_apps", "Weekly OAuth apps"),
    ("seven_day_opus", "Weekly Opus"),
    ("seven_day_sonnet", "Weekly Sonnet"),
];

pub fn parse_usage(body: &[u8]) -> Option<Usage> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let quota = |key: &str, name: &str, period: i64| -> Option<Quota> {
        let q = v.get(key)?;
        Some(Quota {
            name: name.into(),
            percent: q.get("utilization")?.as_f64()?,
            resets_at: q.get("resets_at").and_then(Value::as_str).and_then(parse_time),
            period,
        })
    };
    let mut u = Usage {
        session: quota("five_hour", "Session (5h)", PERIOD_5H),
        weekly: quota("seven_day", "Weekly (7d)", PERIOD_7D),
        others: Vec::new(),
    };
    if u.session.is_none() && u.weekly.is_none() {
        return None;
    }
    for (key, name) in OTHER_QUOTAS {
        if let Some(q) = quota(key, name, PERIOD_7D) {
            u.others.push(q);
        }
    }
    // Model-scoped weekly limits only appear in the "limits" array.
    for l in v.get("limits").and_then(Value::as_array).into_iter().flatten() {
        let Some(model) = l.pointer("/scope/model").and_then(Value::as_str).filter(|m| !m.is_empty()) else {
            continue;
        };
        let mut name = String::from("Weekly ");
        let mut chars = model.chars();
        name.extend(chars.next().map(|c| c.to_ascii_uppercase()));
        name.push_str(chars.as_str());
        if u.others.iter().any(|q| q.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        u.others.push(Quota {
            name,
            percent: l.get("percent").and_then(Value::as_f64).unwrap_or(0.0),
            resets_at: l.get("resets_at").and_then(Value::as_str).and_then(parse_time),
            period: PERIOD_7D,
        });
    }
    Some(u)
}

/// Parses RFC 3339 timestamps like `2026-09-05T12:59:59.966454+00:00` into
/// Unix milliseconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut i = 19;
    let mut ms = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let frac = &s[start..i];
        let digits = frac.len().min(3);
        ms = frac[..digits].parse::<i64>().ok()? * 10_i64.pow(3 - digits as u32);
    }
    let offset_min = match b.get(i) {
        Some(b'Z' | b'z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let oh = num(i + 1..i + 3)?;
            let om = num(i + 4..i + 6)?;
            let o = oh * 60 + om;
            if sign == b'-' { -o } else { o }
        }
        _ => return None,
    };
    let days = days_from_civil(y as i32, mo as u32, d as u32);
    Some(((days * 24 + h) * 60 + mi - offset_min) * 60_000 + sec * 1000 + ms)
}

/// Plausible data for screenshots and testing (`--demo`).
pub fn demo_usage(now: i64) -> Usage {
    use crate::timefmt::MINUTE;
    let weekly_reset = now + 3 * DAY + 4 * HOUR + 20 * MINUTE;
    Usage {
        session: Some(Quota {
            name: "Session (5h)".into(),
            percent: 48.0,
            resets_at: Some(now + 2 * HOUR + 13 * MINUTE),
            period: PERIOD_5H,
        }),
        weekly: Some(Quota { name: "Weekly (7d)".into(), percent: 64.0, resets_at: Some(weekly_reset), period: PERIOD_7D }),
        others: vec![Quota { name: "Weekly Sonnet".into(), percent: 2.0, resets_at: Some(weekly_reset), period: PERIOD_7D }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timefmt::tests::utc;

    const SAMPLE: &str = r#"{
      "five_hour": {"utilization": 48.0, "resets_at": "2026-09-05T12:59:59.966454+00:00"},
      "seven_day": {"utilization": 64.0, "resets_at": "2026-09-11T20:59:59.966479+00:00"},
      "seven_day_opus": null,
      "seven_day_sonnet": {"utilization": 2.0, "resets_at": "2026-09-11T20:59:59.966479+00:00"},
      "nimbus_quill": {"utilization": 0.0, "resets_at": null},
      "limits": [
        {"kind": "session", "percent": 48, "resets_at": "2026-09-05T12:59:59.966454+00:00", "scope": null},
        {"kind": "weekly_model", "percent": 7, "resets_at": "2026-09-11T20:59:59Z", "scope": {"model": "fable"}}
      ]
    }"#;

    #[test]
    fn parses_sample() {
        let u = parse_usage(SAMPLE.as_bytes()).unwrap();
        assert_eq!(u.session.as_ref().unwrap().percent, 48.0);
        let w = u.weekly.unwrap();
        assert_eq!(w.percent, 64.0);
        assert_eq!(w.resets_at, Some(utc(2026, 9, 11, 20, 59) + 59_966));
        let names: Vec<_> = u.others.iter().map(|q| q.name.as_str()).collect();
        assert_eq!(names, ["Weekly Sonnet", "Weekly Fable"]);
    }

    #[test]
    fn session_not_started() {
        let u = parse_usage(br#"{"five_hour": {"utilization": 0, "resets_at": null}, "seven_day": {"utilization": 10, "resets_at": "2026-09-11T20:59:59Z"}}"#).unwrap();
        assert_eq!(u.session.unwrap().resets_at, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_usage(b"{}").is_none());
        assert!(parse_usage(b"not json").is_none());
    }

    #[test]
    fn time_offsets() {
        assert_eq!(parse_time("2026-09-05T14:00:00+02:00"), Some(utc(2026, 9, 5, 12, 0)));
        assert_eq!(parse_time("2026-09-05T12:00:00.5Z"), Some(utc(2026, 9, 5, 12, 0) + 500));
        assert_eq!(parse_time("2026-09-05T07:30:00-04:30"), Some(utc(2026, 9, 5, 12, 0)));
        assert_eq!(parse_time("garbage"), None);
    }
}

/// Parses a saved real response: `USAGE_JSON=path cargo test real_response -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_response() {
    let raw = std::fs::read(std::env::var("USAGE_JSON").expect("set USAGE_JSON")).unwrap();
    let u = parse_usage(&raw).expect("parse");
    for q in u.session.iter().chain(&u.weekly).chain(&u.others) {
        println!("{}: {}% resets_at={:?}", q.name, q.percent, q.resets_at);
    }
    assert!(u.session.is_some() && u.weekly.is_some());
}
