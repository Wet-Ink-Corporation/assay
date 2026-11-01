//! The instants the eligibility rule compares (04 §5): a review's `submitted_at`, a comment's
//! `created_at` and `updated_at`, all RFC 3339 UTC as GitHub writes them, and a push record's
//! `at`, a [`LedgerTime`] read as the same instant.

use std::fmt;
use std::str::FromStr;

use crate::calendar::CalendarDate;
use crate::ledger::LedgerTime;
use crate::status::FetchedAt;

/// Digits of a fraction of a second kept: nanoseconds.
const NANO_DIGITS: u32 = 9;

/// Seconds in an hour and a minute.
const SECONDS_PER_HOUR: u32 = 3_600;
const SECONDS_PER_MINUTE: u32 = 60;

/// Milliseconds in a UTC day and a second, and nanoseconds in a millisecond.
const MILLIS_PER_DAY: u64 = 86_400_000;
const MILLIS_PER_SECOND: u64 = 1_000;
const NANOS_PER_MILLI: u64 = 1_000_000;

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

impl Timestamp {
    /// The same instant as a [`LedgerTime`], or `None` when it does not fall on a millisecond
    /// (never rounded: a command record's `created_at` is compared exactly on every read).
    pub(crate) fn ledger_time(self) -> Option<LedgerTime> {
        let nanos = u64::from(self.nanos);
        if nanos % NANOS_PER_MILLI != 0 {
            return None;
        }
        let millis = u64::from(self.date.days_since_epoch())
            .checked_mul(MILLIS_PER_DAY)?
            .checked_add(u64::from(self.seconds).checked_mul(MILLIS_PER_SECOND)?)?
            .checked_add(nanos / NANOS_PER_MILLI)?;
        LedgerTime::from_unix_millis(millis).ok()
    }
}

impl From<LedgerTime> for Timestamp {
    /// The same instant: a ledger time is a UTC instant to the millisecond in the same range.
    fn from(time: LedgerTime) -> Self {
        let millis = time.unix_millis();
        let of_day = millis % MILLIS_PER_DAY;
        // A `LedgerTime` is at most 9999-12-31 and `of_day` under a day, so no part saturates.
        let days = u32::try_from(millis / MILLIS_PER_DAY).unwrap_or(u32::MAX);
        let seconds = u32::try_from(of_day / MILLIS_PER_SECOND).unwrap_or(u32::MAX);
        let nanos = u32::try_from(of_day % MILLIS_PER_SECOND * NANOS_PER_MILLI).unwrap_or(u32::MAX);
        Self {
            date: CalendarDate::from_days_saturating(days),
            seconds,
            nanos,
        }
    }
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
    use super::{LedgerTime, Timestamp, nanos};

    #[test]
    fn a_fraction_is_scaled_to_nanoseconds() {
        assert_eq!(nanos(""), Some(0));
        assert_eq!(nanos("5"), Some(500_000_000));
        assert_eq!(nanos("000000001"), Some(1));
        assert_eq!(nanos("1234567890"), None, "ten digits are refused");
    }

    #[test]
    fn a_ledger_time_is_the_same_instant() {
        for (ledger, rfc) in [
            ("2026-10-13T09:30:00.250Z", "2026-10-13T09:30:00.25Z"),
            ("1970-01-01T00:00:00.000Z", "1970-01-01T00:00:00Z"),
            ("9999-12-31T23:59:59.999Z", "9999-12-31T23:59:59.999Z"),
        ] {
            let time: LedgerTime = ledger.parse().expect("a canonical ledger time");
            let expected: Timestamp = rfc.parse().expect("an RFC 3339 UTC time");

            assert_eq!(Timestamp::from(time), expected, "{ledger}");
            assert_eq!(Timestamp::from(time).to_string(), rfc, "{ledger}");
        }
    }
}
