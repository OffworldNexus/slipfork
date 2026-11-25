//! The injected local wall clock: one `jiff::tz::TimeZone` value, zero ambient reads.
//!
//! The composition root resolves `$TZ` / `/etc/localtime` once through [`jiff::tz::TimeZone::system`]
//! and hands the resulting value to every renderer. Display modules never call `tzset`,
//! `localtime_r`, or `Utc::now`.

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// One resolved local time zone: a plain value, safe to share and clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallZone {
    zone: TimeZone,
}

impl WallZone {
    /// Wraps one already-resolved time zone without touching the environment.
    #[must_use]
    pub fn from_time_zone(zone: TimeZone) -> Self {
        Self { zone }
    }

    /// A named IANA zone (`Europe/Paris`), or `None` when the host has no such zone.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        TimeZone::get(name).ok().map(Self::from_time_zone)
    }

    /// A fixed UTC offset in seconds, or `None` for an out-of-range offset.
    #[must_use]
    pub fn fixed(offset_seconds: i32) -> Option<Self> {
        jiff::tz::Offset::from_seconds(offset_seconds)
            .ok()
            .map(|offset| Self::from_time_zone(TimeZone::fixed(offset)))
    }

    /// Local `YYYY-MM-DD HH:MM:SS` for one epoch-millisecond instant.
    #[must_use]
    pub fn timestamp(&self, ts_ms: u64) -> String {
        let parts = self.parts(ts_ms);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second
        )
    }

    /// Local `HH:MM:SS.mmm` clock gutter for one epoch-millisecond instant.
    #[must_use]
    pub fn clock(&self, ts_ms: u64) -> String {
        let parts = self.parts(ts_ms);
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            parts.hour, parts.minute, parts.second, parts.millis
        )
    }

    fn parts(&self, ts_ms: u64) -> Parts {
        if let Ok(timestamp) = i64::try_from(ts_ms)
            .map_err(|_| ())
            .and_then(|millis| Timestamp::from_millisecond(millis).map_err(|_| ()))
        {
            let datetime = self.zone.to_datetime(timestamp);
            return Parts {
                year: i128::from(datetime.year()),
                month: datetime.month() as u32,
                day: datetime.day() as u32,
                hour: datetime.hour() as u32,
                minute: datetime.minute() as u32,
                second: datetime.second() as u32,
                millis: datetime.millisecond() as u64,
            };
        }
        raw_epoch(ts_ms)
    }
}

/// One instant resolved to civil fields; an i128 year keeps the raw-epoch fallback total.
struct Parts {
    year: i128,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u64,
}

/// The out-of-range fallback: pure UTC arithmetic from the raw epoch seconds, never a panic.
fn raw_epoch(ts_ms: u64) -> Parts {
    let seconds = i128::from(ts_ms / 1000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let time = u32::try_from(time).unwrap_or(0);
    Parts {
        year,
        month,
        day,
        hour: time / 3600,
        minute: time % 3600 / 60,
        second: time % 60,
        millis: ts_ms % 1000,
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i128) -> (i128, u32, u32) {
    let days = days + 719_468;
    let era = days / 146_097;
    let day_of_era = u32::try_from(days - era * 146_097).unwrap_or(0);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = i128::from(year_of_era) + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::WallZone;

    fn paris() -> WallZone {
        WallZone::named("Europe/Paris").expect("the host carries the IANA zone database")
    }

    #[test]
    fn paris_resolves_both_dst_boundaries_exactly() {
        let zone = paris();
        assert_eq!(
            zone.timestamp(1_774_745_999_000),
            "2026-03-29 01:59:59",
            "one second before the spring-forward gap"
        );
        assert_eq!(
            zone.timestamp(1_774_746_000_000),
            "2026-03-29 03:00:00",
            "the spring-forward gap is skipped"
        );
        assert_eq!(
            zone.timestamp(1_792_889_999_000),
            "2026-10-25 02:59:59",
            "one second before the fall-back repeat"
        );
        assert_eq!(
            zone.timestamp(1_792_890_000_000),
            "2026-10-25 02:00:00",
            "the fall-back repeat is honoured"
        );
    }

    #[test]
    fn paris_summer_and_winter_offsets_and_the_clock_gutter() {
        let zone = paris();
        assert_eq!(zone.timestamp(1_782_907_200_000), "2026-07-01 14:00:00");
        assert_eq!(zone.clock(1_782_907_200_000), "14:00:00.000");
        assert_eq!(zone.timestamp(1_768_478_400_000), "2026-01-15 13:00:00");
    }

    #[test]
    fn a_fixed_offset_shifts_the_same_instant() {
        let zone = WallZone::fixed(3600).expect("one hour is a valid offset");
        assert_eq!(zone.timestamp(1_782_907_200_000), "2026-07-01 13:00:00");
        assert_eq!(zone.clock(1_782_907_200_000), "13:00:00.000");
        assert_eq!(
            WallZone::fixed(100_000),
            None,
            "an offset beyond a day is rejected"
        );
    }

    #[test]
    fn an_out_of_range_instant_never_panics() {
        let zone = WallZone::fixed(0).expect("utc");
        assert!(!zone.timestamp(u64::MAX).is_empty());
        assert_eq!(zone.timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(zone.clock(0), "00:00:00.000");
    }
}
