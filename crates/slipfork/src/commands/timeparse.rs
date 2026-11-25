//! `logs --since/--until` value parsing and the local wall-clock replay gutter.
//!
//! Relative (`30s|5m|2h|1d`) and RFC 3339 inputs both resolve to epoch milliseconds here. The
//! entry point snapshots the process timezone once; replay/`--runs` format against that snapshot
//! with `localtime_r` per instant (DST correct), and no display module reads the environment.

// Refreshes libc's process-timezone cache exactly once, at the composition root.
unsafe extern "C" {
    fn tzset();
}

use slipfork_world::pod_log::now_ms;

/// clap value parser for `--since`: relative (`30s|5m|2h|1d`) or an RFC 3339 timestamp.
pub fn parse_since(text: &str) -> Result<u64, String> {
    parse_time(text)
}

/// clap value parser for `--until`: relative (`30s|5m|2h|1d`) or an RFC 3339 timestamp.
pub fn parse_until(text: &str) -> Result<u64, String> {
    parse_time(text)
}

fn parse_time(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty time value; use `30s|5m|2h|1d` or an RFC 3339 timestamp".to_owned());
    }
    if let Some(delta) = relative_ms(text) {
        return Ok(now_ms().saturating_sub(delta));
    }
    parse_rfc3339_ms(text).ok_or_else(|| {
        format!(
            "`{text}` is not `30s|5m|2h|1d` or RFC 3339 like `2026-11-22T12:00:00Z` \
             (next: `slipfork logs api --since 5m`)"
        )
    })
}

fn relative_ms(text: &str) -> Option<u64> {
    if !text.is_ascii() {
        return None;
    }
    let (number, unit) = text.split_at(text.len().checked_sub(1)?);
    let factor = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    number.parse::<u64>().ok()?.checked_mul(factor)
}

fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let text = text.trim();
    if !text.is_ascii() || text.len() < 20 {
        return None;
    }
    let year = i64::from(digits(&text[0..4])?);
    if &text[4..5] != "-" || &text[7..8] != "-" {
        return None;
    }
    let month = digits(&text[5..7])?;
    let day = digits(&text[8..10])?;
    if !matches!(text.as_bytes()[10], b'T' | b't' | b' ') {
        return None;
    }
    if &text[13..14] != ":" || &text[16..17] != ":" {
        return None;
    }
    let hour = digits(&text[11..13])?;
    let minute = digits(&text[14..16])?;
    let second = digits(&text[17..19])?;
    if month == 0
        || month > 12
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut rest = &text[19..];
    let mut frac = 0_u64;
    if let Some(stripped) = rest.strip_prefix('.') {
        let fraction: String = stripped
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        if fraction.is_empty() {
            return None;
        }
        let mut padded = fraction.clone();
        padded.truncate(3);
        while padded.len() < 3 {
            padded.push('0');
        }
        frac = padded.parse().ok()?;
        rest = &stripped[fraction.len()..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0_i64,
        _ => {
            let bytes = rest.as_bytes();
            let sign = match bytes[0] {
                b'+' => 1_i64,
                b'-' => -1_i64,
                _ => return None,
            };
            let body = &rest[1..];
            let (hours, minutes) = if body.len() == 5 && body.as_bytes()[2] == b':' {
                (&body[0..2], &body[3..5])
            } else if body.len() == 4 {
                (&body[0..2], &body[2..4])
            } else {
                return None;
            };
            let hours = i64::from(digits(hours)?);
            let minutes = i64::from(digits(minutes)?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    let seconds =
        days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)
            - offset;
    if seconds < 0 {
        return None;
    }
    Some(seconds as u64 * 1000 + frac)
}

fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = (days - era * 146_097) as u32;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = i64::from(year_of_era) + era * 400;
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

/// The wall clock a human surface prints, resolved once by the composition root.
///
/// Display modules receive this value and never read `TZ`; [`LocalClock::System`] asks
/// `localtime_r` per instant (so DST is correct), while [`LocalClock::Fixed`] is a deterministic
/// offset for tests. The daemon never constructs either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalClock {
    /// The process timezone, resolved per instant by `localtime_r`.
    System,
    /// A fixed UTC offset in seconds, used by pure tests.
    #[cfg_attr(not(test), allow(dead_code))]
    Fixed(i32),
}

impl LocalClock {
    /// The one clock the composition root resolves for this invocation.
    ///
    /// `tzset` snapshots the process timezone once; `localtime_r` then formats each instant against
    /// that cached zone (DST correct per instant). No renderer reads the environment.
    pub(crate) fn resolve() -> Self {
        // SAFETY: `tzset` only refreshes libc's process-timezone cache; called once at the root.
        unsafe {
            tzset();
        }
        Self::System
    }

    /// A deterministic offset clock (test-only), so no test mutates the process timezone.
    #[cfg(test)]
    pub(crate) const fn fixed(offset_seconds: i32) -> Self {
        Self::Fixed(offset_seconds)
    }

    /// Local `HH:MM:SS.mmm` clock gutter for a replayed run step.
    pub(crate) fn clock(self, ts_ms: u64) -> String {
        let millis = ts_ms % 1000;
        match self.parts(ts_ms) {
            Some(parts) => format!(
                "{:02}:{:02}:{:02}.{millis:03}",
                parts.hour, parts.minute, parts.second
            ),
            None => utc_clock(ts_ms),
        }
    }

    /// Local `YYYY-MM-DD HH:MM:SS` gutter for `-t`, using the same wall clock as [`Self::clock`].
    pub(crate) fn timestamp(self, ts_ms: u64) -> String {
        match self.parts(ts_ms) {
            Some(parts) => format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second
            ),
            None => utc_timestamp(ts_ms),
        }
    }

    fn parts(self, ts_ms: u64) -> Option<LocalParts> {
        match self {
            Self::System => system_parts(ts_ms),
            Self::Fixed(offset) => fixed_parts(ts_ms, offset),
        }
    }
}

/// One timestamp resolved against the process timezone.
struct LocalParts {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

/// Resolves `ts_ms` through `localtime_r`; `None` only for an unrepresentable epoch.
fn system_parts(ts_ms: u64) -> Option<LocalParts> {
    let seconds = libc::time_t::try_from(ts_ms / 1000).ok()?;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: `localtime_r` writes a fully initialized `tm` through the pointer on success.
    let result = unsafe { libc::localtime_r(&seconds, tm.as_mut_ptr()) };
    if result.is_null() {
        return None;
    }
    // SAFETY: the successful call above initialized the whole struct.
    let tm = unsafe { tm.assume_init() };
    Some(LocalParts {
        year: tm.tm_year + 1900,
        month: u32::try_from(tm.tm_mon + 1).ok()?,
        day: u32::try_from(tm.tm_mday).ok()?,
        hour: u32::try_from(tm.tm_hour).ok()?,
        minute: u32::try_from(tm.tm_min).ok()?,
        second: u32::try_from(tm.tm_sec).ok()?,
    })
}

/// Pure civil conversion at a fixed offset; the test path never touches the environment.
fn fixed_parts(ts_ms: u64, offset_seconds: i32) -> Option<LocalParts> {
    let seconds = i64::try_from(ts_ms / 1000)
        .ok()?
        .saturating_add(i64::from(offset_seconds));
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    Some(LocalParts {
        year: i32::try_from(year).ok()?,
        month,
        day,
        hour: u32::try_from(time / 3600).ok()?,
        minute: u32::try_from(time % 3600 / 60).ok()?,
        second: u32::try_from(time % 60).ok()?,
    })
}

fn utc_clock(ts_ms: u64) -> String {
    let millis = ts_ms % 1000;
    let time = (ts_ms / 1000).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}.{millis:03}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

fn utc_timestamp(ts_ms: u64) -> String {
    let seconds = (ts_ms / 1000) as i64;
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::LocalClock;

    #[test]
    fn fixed_offset_wall_clock_respects_the_paris_summer_and_winter_offsets() {
        // 2026-07-01T12:00:00Z is CEST (UTC+2); 2026-01-15T12:00:00Z is CET (UTC+1).
        let summer = LocalClock::fixed(2 * 3600);
        assert_eq!(summer.timestamp(1_782_907_200_000), "2026-07-01 14:00:00");
        assert_eq!(summer.clock(1_782_907_200_000), "14:00:00.000");
        let winter = LocalClock::fixed(3600);
        assert_eq!(winter.timestamp(1_768_478_400_000), "2026-01-15 13:00:00");
        assert_eq!(winter.clock(1_768_478_400_000), "13:00:00.000");
    }
}
