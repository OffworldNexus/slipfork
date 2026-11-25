//! `logs --since/--until` value parsing and the local wall-clock replay gutter.
//!
//! Relative (`30s|5m|2h|1d`) and RFC 3339 inputs both resolve to epoch milliseconds here. The
//! composition root resolves the local wall clock once into a [`WallZone`]; display modules format
//! against that value and never read the environment.

use slipfork_world::pod_log::now_ms;

use crate::ui::report::WallZone;

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

/// The wall clock a human surface prints, resolved once by the composition root.
///
/// A thin delegate over [`WallZone`]: display modules receive this value and never read `TZ`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalClock(WallZone);

impl LocalClock {
    /// Wraps the one wall clock the composition root resolved for this invocation.
    pub(crate) fn new(zone: WallZone) -> Self {
        Self(zone)
    }

    /// The underlying wall zone, for renderers constructed from this clock.
    pub(crate) fn zone(&self) -> &WallZone {
        &self.0
    }

    /// Local `HH:MM:SS.mmm` clock gutter for a replayed run step.
    #[cfg(test)]
    pub(crate) fn clock(&self, ts_ms: u64) -> String {
        self.0.clock(ts_ms)
    }

    /// Local `YYYY-MM-DD HH:MM:SS` gutter for `-t`, using the same wall clock as [`Self::clock`].
    pub(crate) fn timestamp(&self, ts_ms: u64) -> String {
        self.0.timestamp(ts_ms)
    }

    /// A deterministic offset clock (test-only), so no test mutates the process timezone.
    #[cfg(test)]
    pub(crate) fn fixed(offset_seconds: i32) -> Self {
        Self(WallZone::fixed(offset_seconds).expect("a whole-hour test offset is valid"))
    }
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
