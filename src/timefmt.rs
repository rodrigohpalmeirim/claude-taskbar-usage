//! Time math and formatting. Pure code: the time zone and locale come in
//! through the [`Locale`] trait, so all of this is unit-testable on any OS.
//!
//! Instants are milliseconds since the Unix epoch (UTC).

use crate::usage::Quota;

pub const MINUTE: i64 = 60_000;
pub const HOUR: i64 = 60 * MINUTE;
pub const DAY: i64 = 24 * HOUR;

/// A local calendar time, minute precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    /// 0 = Sunday
    pub weekday: u32,
}

pub trait Locale {
    /// Offset of local time from UTC at the given instant, in ms.
    fn utc_offset(&self, utc_ms: i64) -> i64;
    /// The instant of local midnight on the given date (DST-aware).
    fn local_midnight(&self, year: i32, month: u32, day: u32) -> i64;
    /// "14:59" / "2:59 PM", following the user's settings.
    fn clock(&self, t: &Civil) -> String;
    /// Abbreviated weekday name: "Fri".
    fn weekday(&self, t: &Civil) -> String;
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// Splits a "local milliseconds" value (UTC instant + offset) into fields.
pub fn split_local(local_ms: i64) -> Civil {
    let days = local_ms.div_euclid(DAY);
    let rem = local_ms.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: (rem / HOUR) as u32,
        minute: (rem % HOUR / MINUTE) as u32,
        weekday: (days + 4).rem_euclid(7) as u32, // 1970-01-01 was a Thursday
    }
}

pub fn to_local(utc_ms: i64, loc: &dyn Locale) -> Civil {
    split_local(utc_ms + loc.utc_offset(utc_ms))
}

/// The API reports resets like 20:59:59.97; show them as 21:00.
pub fn round_minute(ms: i64) -> i64 {
    (ms + 30_000).div_euclid(MINUTE) * MINUTE
}

/// Time left until a reset: "42m", "3h 05m", "2d 4h".
pub fn countdown(ms: i64) -> String {
    if ms <= 0 {
        return "now".into();
    }
    let mins = (ms + MINUTE - 1) / MINUTE; // round up
    if mins < 60 {
        format!("{mins}m")
    } else if mins < 24 * 60 {
        format!("{}h {:02}m", mins / 60, mins % 60)
    } else {
        let hours = mins / 60;
        format!("{}d {}h", hours / 24, hours % 24)
    }
}

fn same_day(a: &Civil, b: &Civil) -> bool {
    (a.year, a.month, a.day) == (b.year, b.month, b.day)
}

/// When a reset happens: "14:59" today, otherwise "Fri 21:00".
pub fn reset_at(reset: i64, now: i64, loc: &dyn Locale) -> String {
    let r = to_local(round_minute(reset), loc);
    let n = to_local(now, loc);
    if same_day(&r, &n) { loc.clock(&r) } else { format!("{} {}", loc.weekday(&r), loc.clock(&r)) }
}

/// Tooltip variant: "today 14:59", "tomorrow 09:00", "Fri 21:00".
pub fn reset_at_long(reset: i64, now: i64, loc: &dyn Locale) -> String {
    let r = to_local(round_minute(reset), loc);
    let n = to_local(now, loc);
    let tomorrow = split_local(days_from_civil(n.year, n.month, n.day) * DAY + DAY);
    if same_day(&r, &n) {
        format!("today {}", loc.clock(&r))
    } else if same_day(&r, &tomorrow) {
        format!("tomorrow {}", loc.clock(&r))
    } else {
        format!("{} {}", loc.weekday(&r), loc.clock(&r))
    }
}

/// How far "now" is into the quota window (0..=1); None if it hasn't started.
pub fn elapsed_fraction(q: &Quota, now: i64) -> Option<f64> {
    let reset = q.resets_at?;
    if q.period <= 0 {
        return None;
    }
    let f = 1.0 - (reset - now) as f64 / q.period as f64;
    Some(f.clamp(0.0, 1.0))
}

/// Divider positions (0..1, exclusive) inside a quota bar: hour marks for
/// the 5-hour window, local midnights for windows of a day or longer.
pub fn ticks(q: &Quota, loc: &dyn Locale) -> Vec<f64> {
    if q.period <= 0 {
        return Vec::new();
    }
    if q.period < DAY {
        let hours = q.period / HOUR;
        if !(2..=12).contains(&hours) {
            return Vec::new();
        }
        return (1..hours).map(|i| i as f64 / hours as f64).collect();
    }
    let Some(end) = q.resets_at else {
        // Window not started: evenly spaced day marks.
        let days = q.period / DAY;
        return (1..days).map(|i| i as f64 / days as f64).collect();
    };
    let start = end - q.period;
    let s = to_local(start, loc);
    let first_day = days_from_civil(s.year, s.month, s.day);
    let mut out = Vec::new();
    // Each midnight comes from its own calendar date, so a DST change inside
    // the window keeps every mark on a true local midnight.
    for d in 1.. {
        let (y, m, dd) = civil_from_days(first_day + d);
        let midnight = loc.local_midnight(y, m, dd);
        if midnight >= end {
            break;
        }
        let rel = (midnight - start) as f64 / q.period as f64;
        if rel > 0.003 && rel < 0.997 {
            out.push(rel);
        }
    }
    out
}

/// Usage ahead of the clock (or exhausted).
pub fn outpacing(q: &Quota, now: i64) -> bool {
    q.percent >= 100.0 || elapsed_fraction(q, now).is_some_and(|f| q.percent > f * 100.0)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Fixed-offset locale with 24-hour English formatting.
    pub struct Fixed(pub i64);

    impl Locale for Fixed {
        fn utc_offset(&self, _: i64) -> i64 {
            self.0
        }
        fn local_midnight(&self, y: i32, m: u32, d: u32) -> i64 {
            days_from_civil(y, m, d) * DAY - self.0
        }
        fn clock(&self, t: &Civil) -> String {
            format!("{:02}:{:02}", t.hour, t.minute)
        }
        fn weekday(&self, t: &Civil) -> String {
            ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][t.weekday as usize].into()
        }
    }

    pub fn utc(y: i32, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, mo, d) * DAY + h * HOUR + mi * MINUTE
    }

    #[test]
    fn civil_roundtrip() {
        for z in [-1000, 0, 1, 20_000, 20_700, 100_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(split_local(utc(2026, 9, 11, 21, 0)).weekday, 5); // Friday
    }

    #[test]
    fn countdown_format() {
        assert_eq!(countdown(-1), "now");
        assert_eq!(countdown(30_000), "1m");
        assert_eq!(countdown(42 * MINUTE + 10_000), "43m");
        assert_eq!(countdown(3 * HOUR + 5 * MINUTE), "3h 05m");
        assert_eq!(countdown(2 * DAY + 4 * HOUR), "2d 4h");
    }

    #[test]
    fn reset_rounds_up_and_names_day() {
        let loc = Fixed(0);
        let now = utc(2026, 9, 5, 10, 0);
        assert_eq!(reset_at(utc(2026, 9, 5, 12, 59) + 59_900, now, &loc), "13:00");
        assert_eq!(reset_at(utc(2026, 9, 11, 20, 59) + 59_000, now, &loc), "Fri 21:00");
        assert_eq!(reset_at_long(utc(2026, 9, 6, 9, 0), now, &loc), "tomorrow 09:00");
    }

    #[test]
    fn offset_changes_the_day() {
        // 23:30 UTC is already the next day at UTC+2.
        let loc = Fixed(2 * HOUR);
        let now = utc(2026, 9, 5, 12, 0);
        assert_eq!(reset_at(utc(2026, 9, 5, 23, 30), now, &loc), "Sun 01:30");
    }

    #[test]
    fn hour_and_day_ticks() {
        let five = Quota { name: String::new(), percent: 0.0, resets_at: Some(HOUR), period: 5 * HOUR };
        assert_eq!(ticks(&five, &Fixed(0)), vec![0.2, 0.4, 0.6, 0.8]);

        let end = utc(2026, 9, 11, 21, 0);
        let week = Quota { name: String::new(), percent: 0.0, resets_at: Some(end), period: 7 * DAY };
        let t = ticks(&week, &Fixed(0));
        assert_eq!(t.len(), 7);
        let start = end - 7 * DAY;
        assert_eq!(t[0], (utc(2026, 9, 5, 0, 0) - start) as f64 / (7 * DAY) as f64);
    }

    #[test]
    fn outpacing_compares_with_elapsed_time() {
        let now = 10 * DAY;
        let mut q = Quota { name: String::new(), percent: 30.0, resets_at: Some(now + 4 * HOUR), period: 5 * HOUR };
        assert!(outpacing(&q, now)); // 20% elapsed
        q.percent = 10.0;
        assert!(!outpacing(&q, now));
        q.resets_at = None;
        assert_eq!(elapsed_fraction(&q, now), None);
    }
}
