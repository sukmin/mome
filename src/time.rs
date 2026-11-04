//! Local wall-clock time without pulling in a date crate.
//!
//! Revision file names, session ids and the status bar all use local time, so the
//! value comes straight from `GetLocalTime`. `to_seconds` gives a monotonic-enough
//! number for age comparisons (it treats local time as if it were UTC).

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_NOSECONDS};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

/// Current time and date in the user's short format, like Notepad's F5 ("오후 3:45 2026-10-06").
pub fn time_date_string() -> String {
    /// Calls a `Get*FormatEx` function with a buffer; empty on failure.
    fn format(call: impl Fn(*mut u16, i32) -> i32) -> String {
        let mut buf = [0u16; 128];
        let n = call(buf.as_mut_ptr(), buf.len() as i32);
        if n <= 1 {
            return String::new();
        }
        // `n` includes the terminating NUL.
        String::from_utf16_lossy(&buf[..(n - 1) as usize])
    }
    use std::ptr::null;
    // Null locale, time and format: the user's locale, now, and their short format.
    let time = format(|buf, len| unsafe { GetTimeFormatEx(null(), TIME_NOSECONDS, null(), null(), buf, len) });
    let date =
        format(|buf, len| unsafe { GetDateFormatEx(null(), DATE_SHORTDATE, null(), null(), buf, len, null()) });
    if time.is_empty() && date.is_empty() {
        LocalTime::now().display()
    } else {
        format!("{time} {date}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millis: u16,
}

impl LocalTime {
    pub fn now() -> Self {
        let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut st) };
        Self {
            year: st.wYear,
            month: st.wMonth as u8,
            day: st.wDay as u8,
            hour: st.wHour as u8,
            minute: st.wMinute as u8,
            second: st.wSecond as u8,
            millis: st.wMilliseconds,
        }
    }

    /// `20261006-153012-123`, sortable and safe for file names.
    pub fn stamp(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
        )
    }

    pub fn parse_stamp(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 19 || b[8] != b'-' || b[15] != b'-' {
            return None;
        }
        let num = |range: std::ops::Range<usize>| -> Option<u32> {
            let part = s.get(range)?;
            if !part.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let t = Self {
            year: num(0..4)? as u16,
            month: num(4..6)? as u8,
            day: num(6..8)? as u8,
            hour: num(9..11)? as u8,
            minute: num(11..13)? as u8,
            second: num(13..15)? as u8,
            millis: num(16..19)? as u16,
        };
        let valid = (1..=12).contains(&t.month)
            && (1..=31).contains(&t.day)
            && t.hour < 24
            && t.minute < 60
            && t.second < 61;
        valid.then_some(t)
    }

    /// Seconds since 1970-01-01 as if this local time were UTC.
    pub fn to_seconds(self) -> i64 {
        let days = days_from_civil(self.year as i64, self.month as i64, self.day as i64);
        days * 86_400 + self.hour as i64 * 3_600 + self.minute as i64 * 60 + self.second as i64
    }

    pub fn date_string(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub fn time_string(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// `2026-10-06 15:30:12`
    pub fn display(&self) -> String {
        format!("{} {}", self.date_string(), self.time_string())
    }

    /// Short Korean description of how long ago this was, relative to `now`.
    pub fn ago(&self, now: &LocalTime) -> String {
        let secs = now.to_seconds() - self.to_seconds();
        match secs {
            s if s < 10 => "방금".to_owned(),
            s if s < 60 => format!("{s}초 전"),
            s if s < 3_600 => format!("{}분 전", s / 60),
            s if s < 86_400 => format!("{}시간 전", s / 3_600),
            s => format!("{}일 전", s / 86_400),
        }
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_round_trip() {
        let t = LocalTime {
            year: 2026,
            month: 10,
            day: 6,
            hour: 15,
            minute: 30,
            second: 12,
            millis: 7,
        };
        assert_eq!(t.stamp(), "20261006-153012-007");
        assert_eq!(LocalTime::parse_stamp(&t.stamp()), Some(t));
        assert_eq!(LocalTime::parse_stamp("2026100-153012-007"), None);
        assert_eq!(LocalTime::parse_stamp("20261306-153012-007"), None);
    }

    #[test]
    fn seconds_are_consistent() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        let a = LocalTime::parse_stamp("20261231-235959-000").unwrap();
        let b = LocalTime::parse_stamp("20270101-000000-000").unwrap();
        assert_eq!(b.to_seconds() - a.to_seconds(), 1);
    }
}
