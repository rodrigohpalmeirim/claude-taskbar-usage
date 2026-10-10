//! Time zone conversion and clock/weekday formatting through the Win32 NLS
//! API, so DST rules, 12/24-hour clock and weekday names follow Windows.

use super::from_wide;
use crate::timefmt::{Civil, DAY, HOUR, Locale, MINUTE, days_from_civil, split_local};
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
use windows_sys::Win32::System::Time::{
    DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx, TzSpecificLocalTimeToSystemTimeEx,
};
use windows_sys::w;

pub struct WinLocale;

fn st_from_ms(ms: i64) -> SYSTEMTIME {
    let c = split_local(ms);
    let rem = ms.rem_euclid(MINUTE);
    SYSTEMTIME {
        wYear: c.year as u16,
        wMonth: c.month as u16,
        wDayOfWeek: c.weekday as u16,
        wDay: c.day as u16,
        wHour: c.hour as u16,
        wMinute: c.minute as u16,
        wSecond: (rem / 1000) as u16,
        wMilliseconds: (rem % 1000) as u16,
    }
}

fn ms_from_st(st: &SYSTEMTIME) -> i64 {
    days_from_civil(i32::from(st.wYear), u32::from(st.wMonth), u32::from(st.wDay)) * DAY
        + i64::from(st.wHour) * HOUR
        + i64::from(st.wMinute) * MINUTE
        + i64::from(st.wSecond) * 1000
        + i64::from(st.wMilliseconds)
}

fn st_from_civil(t: &Civil) -> SYSTEMTIME {
    SYSTEMTIME {
        wYear: t.year as u16,
        wMonth: t.month as u16,
        wDayOfWeek: t.weekday as u16,
        wDay: t.day as u16,
        wHour: t.hour as u16,
        wMinute: t.minute as u16,
        wSecond: 0,
        wMilliseconds: 0,
    }
}

fn time_zone() -> DYNAMIC_TIME_ZONE_INFORMATION {
    // SAFETY: plain-data struct filled in by the call.
    unsafe {
        let mut tz = std::mem::zeroed();
        GetDynamicTimeZoneInformation(&mut tz);
        tz
    }
}

impl Locale for WinLocale {
    fn utc_offset(&self, utc_ms: i64) -> i64 {
        let tz = time_zone();
        let utc = st_from_ms(utc_ms);
        let mut local = utc;
        // SAFETY: valid pointers to initialised structs.
        if unsafe { SystemTimeToTzSpecificLocalTimeEx(&tz, &utc, &mut local) } == 0 {
            return 0;
        }
        ms_from_st(&local) - ms_from_st(&utc)
    }

    fn local_midnight(&self, year: i32, month: u32, day: u32) -> i64 {
        let tz = time_zone();
        let local = st_from_civil(&Civil { year, month, day, hour: 0, minute: 0, weekday: 0 });
        let mut utc = local;
        // SAFETY: valid pointers to initialised structs.
        if unsafe { TzSpecificLocalTimeToSystemTimeEx(&tz, &local, &mut utc) } == 0 {
            let naive = days_from_civil(year, month, day) * DAY;
            return naive - self.utc_offset(naive);
        }
        ms_from_st(&utc)
    }

    fn clock(&self, t: &Civil) -> String {
        let st = st_from_civil(t);
        let mut buf = [0u16; 64];
        // SAFETY: buffer length matches; NULL locale = user default.
        let n = unsafe { GetTimeFormatEx(std::ptr::null(), TIME_NOSECONDS, &st, std::ptr::null(), buf.as_mut_ptr(), buf.len() as i32) };
        if n == 0 { format!("{:02}:{:02}", t.hour, t.minute) } else { from_wide(&buf) }
    }

    fn weekday(&self, t: &Civil) -> String {
        let st = st_from_civil(t);
        let mut buf = [0u16; 64];
        // SAFETY: buffer length matches; NULL locale = user default.
        let n = unsafe { GetDateFormatEx(std::ptr::null(), 0, &st, w!("ddd"), buf.as_mut_ptr(), buf.len() as i32, std::ptr::null()) };
        if n == 0 { ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][t.weekday as usize % 7].into() } else { from_wide(&buf) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timefmt::to_local;

    #[test]
    fn midnights_are_local_midnights() {
        let loc = WinLocale;
        // Spans both DST changes for zones that have them.
        for (y, m, d) in [(2026, 1, 15), (2026, 3, 29), (2026, 7, 1), (2026, 10, 25), (2026, 11, 1)] {
            let t = to_local(loc.local_midnight(y, m, d), &loc);
            assert_eq!((t.year, t.month, t.day, t.hour, t.minute), (y, m, d, 0, 0));
        }
    }

    #[test]
    fn formats_something() {
        let t = Civil { year: 2026, month: 9, day: 11, hour: 21, minute: 0, weekday: 5 };
        assert!(WinLocale.clock(&t).contains('9') || WinLocale.clock(&t).contains("21"));
        assert!(!WinLocale.weekday(&t).is_empty());
    }
}
