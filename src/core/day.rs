//! Local date and time from UTC plus the fixed offset in the config.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeDelta, Timelike, Utc, Weekday};

/// A clock that goes back by up to this much is ignored: we keep the last seen time.
/// A bigger jump is taken as a real clock fix (for example a wrong BIOS clock).
pub const MAX_CLOCK_BACK: TimeDelta = TimeDelta::hours(48);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNow {
    pub date: NaiveDate,
    pub weekday: Weekday,
    /// Minutes after midnight.
    pub minute: u32,
    /// Seconds into the current minute.
    pub second: u32,
}

/// The UTC time to use. If the clock went back a little, the last seen time is used.
pub fn effective_utc(now: DateTime<Utc>, last_seen: Option<DateTime<Utc>>) -> DateTime<Utc> {
    match last_seen {
        Some(last) if now < last && last - now <= MAX_CLOCK_BACK => last,
        _ => now,
    }
}

pub fn local_now(utc: DateTime<Utc>, offset_minutes: i32) -> LocalNow {
    let offset =
        FixedOffset::east_opt(offset_minutes * 60).unwrap_or(FixedOffset::east_opt(0).unwrap());
    let local = utc.with_timezone(&offset);
    LocalNow {
        date: local.date_naive(),
        weekday: local.weekday(),
        minute: local.hour() * 60 + local.minute(),
        second: local.second(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn offset_moves_the_date() {
        // 22:30 UTC on Monday is 01:30 on Tuesday at UTC+3.
        let now = local_now(utc(2026, 10, 19, 22, 30), 180);
        assert_eq!(now.date, NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        assert_eq!(now.weekday, Weekday::Tue);
        assert_eq!(now.minute, 90);
    }

    #[test]
    fn negative_offset() {
        let now = local_now(utc(2026, 10, 20, 2, 0), -300);
        assert_eq!(now.date, NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        assert_eq!(now.minute, 21 * 60);
    }

    #[test]
    fn clock_going_forward_is_used() {
        let last = utc(2026, 10, 20, 12, 0);
        assert_eq!(
            effective_utc(utc(2026, 10, 20, 12, 1), Some(last)),
            utc(2026, 10, 20, 12, 1)
        );
    }

    #[test]
    fn small_clock_change_back_is_ignored() {
        let last = utc(2026, 10, 21, 0, 10);
        // Back to the previous day: keep the last seen time, so the day does not change.
        assert_eq!(effective_utc(utc(2026, 10, 20, 23, 0), Some(last)), last);
    }

    #[test]
    fn big_clock_fix_is_used() {
        let last = utc(2030, 1, 1, 0, 0);
        let now = utc(2026, 10, 20, 12, 0);
        assert_eq!(effective_utc(now, Some(last)), now);
    }

    #[test]
    fn no_last_seen() {
        let now = utc(2026, 10, 20, 12, 0);
        assert_eq!(effective_utc(now, None), now);
    }
}
