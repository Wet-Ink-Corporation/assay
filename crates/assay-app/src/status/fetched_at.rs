//! When a status was read: the caller's clock reading, parsed before anything is read so a bad
//! one is the caller's error rather than a document that fails its own schema.

use std::fmt;
use std::str::FromStr;

/// The longest fraction of a second accepted, in digits (nanoseconds).
const MAX_FRACTION_DIGITS: usize = 9;

/// An RFC 3339 date-time in UTC, as `status` writes `fetched_at` (`2026-10-13T09:30:00Z`).
///
/// `YYYY-MM-DDTHH:MM:SS`, an optional fraction of one to nine digits, then `Z`. Upper-case `T`
/// and `Z` only, no offset and no leap second: a subset of the schema's `date-time`, so every
/// value accepted here passes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FetchedAt(String);

impl FetchedAt {
    /// The date-time as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FetchedAt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A `fetched_at` that is not an RFC 3339 UTC date-time [`FetchedAt`] accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the read time is not an RFC 3339 UTC date-time such as 2026-10-13T09:30:00Z")]
pub struct InvalidFetchedAt;

impl FromStr for FetchedAt {
    type Err = InvalidFetchedAt;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if is_utc_date_time(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(InvalidFetchedAt)
        }
    }
}

/// Whether `text` is `YYYY-MM-DDTHH:MM:SS[.f{1,9}]Z` naming a real day and time.
fn is_utc_date_time(text: &str) -> bool {
    let Some((date, time)) = text.split_once('T') else {
        return false;
    };
    let Some(time) = time.strip_suffix('Z') else {
        return false;
    };
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let fraction_reads = fraction.is_none_or(|digits| {
        (1..=MAX_FRACTION_DIGITS).contains(&digits.len())
            && digits.bytes().all(|byte| byte.is_ascii_digit())
    });
    fraction_reads && is_date(date) && is_clock(clock)
}

/// Whether `date` is `YYYY-MM-DD` naming a day of the proleptic Gregorian calendar.
fn is_date(date: &str) -> bool {
    let mut parts = date.split('-');
    let (Some(year), Some(month), Some(day), None) = (
        parts.next().and_then(|part| number(part, 4)),
        parts.next().and_then(|part| number(part, 2)),
        parts.next().and_then(|part| number(part, 2)),
        parts.next(),
    ) else {
        return false;
    };
    days_in(year, month).is_some_and(|days| (1..=days).contains(&day))
}

/// Whether `clock` is `HH:MM:SS` within one day, without a leap second.
fn is_clock(clock: &str) -> bool {
    let mut parts = clock.split(':');
    let (Some(hour), Some(minute), Some(second), None) = (
        parts.next().and_then(|part| number(part, 2)),
        parts.next().and_then(|part| number(part, 2)),
        parts.next().and_then(|part| number(part, 2)),
        parts.next(),
    ) else {
        return false;
    };
    hour <= 23 && minute <= 59 && second <= 59
}

/// `text` as a number when it is exactly `width` ASCII digits.
fn number(text: &str, width: usize) -> Option<u32> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// How many days `month` of `year` has; `None` for a month that does not exist.
fn days_in(year: u32, month: u32) -> Option<u32> {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if leap => Some(29),
        2 => Some(28),
        _ => None,
    }
}
