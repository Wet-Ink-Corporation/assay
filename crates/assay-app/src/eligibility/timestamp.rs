//! The instants the eligibility rule compares (04 §5): a review's `submitted_at`, a comment's
//! `created_at` and `updated_at`, and a push record's `at`, all RFC 3339 UTC as GitHub writes them.

use std::fmt;
use std::str::FromStr;

use crate::calendar::CalendarDate;
use crate::status::FetchedAt;

/// Digits of a fraction of a second kept: nanoseconds.
const NANO_DIGITS: u32 = 9;

/// Seconds in an hour and a minute.
const SECONDS_PER_HOUR: u32 = 3_600;
const SECONDS_PER_MINUTE: u32 = 60;

/// An RFC 3339 date-time in UTC (`2026-10-13T09:30:00Z`, an optional fraction of one to nine
/// digits), from 1970 to 9999.
///
/// Equal and ordered by the instant it names, so `09:30:00.5Z` and `09:30:00.500Z` are one
/// time. [`fmt::Display`] writes it canonically: the fraction without trailing zeros, none when
/// it is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp {
    // Field order is the comparison order.
    date: CalendarDate,
    seconds: u32,
    nanos: u32,
}

/// A text that is not an RFC 3339 UTC date-time [`Timestamp`] accepts. The text is never shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the time is not an RFC 3339 UTC date-time such as 2026-10-13T09:30:00Z")]
pub struct InvalidTimestamp;

impl FromStr for Timestamp {
    type Err = InvalidTimestamp;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        // `FetchedAt` checks the whole shape; what is left only needs splitting.
        text.parse::<FetchedAt>()
            .map_err(|_shape| InvalidTimestamp)?;
        let (date, time) = text.split_once('T').ok_or(InvalidTimestamp)?;
        let time = time.strip_suffix('Z').ok_or(InvalidTimestamp)?;
        let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
        let date = date
            .parse::<CalendarDate>()
            .map_err(|_range| InvalidTimestamp)?;
        let seconds = clock.split(':').try_fold(0_u32, |total, part| {
            let part: u32 = part.parse().ok()?;
            total.checked_mul(SECONDS_PER_MINUTE)?.checked_add(part)
        });
        Ok(Self {
            date,
            seconds: seconds.ok_or(InvalidTimestamp)?,
            nanos: nanos(fraction).ok_or(InvalidTimestamp)?,
        })
    }
}

/// A fraction's digits (at most nine, already checked) as nanoseconds; `""` is zero.
fn nanos(fraction: &str) -> Option<u32> {
    let digits = u32::try_from(fraction.len()).ok()?;
    let scale = 10_u32.checked_pow(NANO_DIGITS.checked_sub(digits)?)?;
    let value = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u32>().ok()?
    };
    value.checked_mul(scale)
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hour = self.seconds / SECONDS_PER_HOUR;
        let minute = self.seconds % SECONDS_PER_HOUR / SECONDS_PER_MINUTE;
        let second = self.seconds % SECONDS_PER_MINUTE;
        write!(f, "{}T{hour:02}:{minute:02}:{second:02}", self.date)?;
        if self.nanos != 0 {
            let digits = format!("{:09}", self.nanos);
            write!(f, ".{}", digits.trim_end_matches('0'))?;
        }
        f.write_str("Z")
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::nanos;

    #[test]
    fn a_fraction_is_scaled_to_nanoseconds() {
        assert_eq!(nanos(""), Some(0));
        assert_eq!(nanos("5"), Some(500_000_000));
        assert_eq!(nanos("000000001"), Some(1));
        assert_eq!(nanos("1234567890"), None, "ten digits are refused");
    }
}
