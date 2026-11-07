//! When a status was read: the caller's clock reading, parsed before anything is read so a bad
//! one is the caller's error rather than a document that fails its own schema.

use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use crate::calendar::{CalendarDate, CalendarDateError};

/// The longest fraction of a second accepted, in digits (nanoseconds).
const MAX_FRACTION_DIGITS: usize = 9;

/// Seconds in a UTC day, an hour and a minute (no leap seconds, as Unix time counts).
const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;

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

    /// `time` written in UTC to the millisecond (`2026-10-13T09:30:00.123Z`): how the CLI
    /// stamps `fetched_at` and each event's `ts`.
    ///
    /// # Errors
    ///
    /// [`CalendarDateError::OutOfRange`] when `time` is before 1970-01-01 or after 9999-12-31.
    pub fn from_system_time(time: SystemTime) -> Result<Self, CalendarDateError> {
        let date = CalendarDate::from_system_time(time)?;
        let since = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_before| CalendarDateError::OutOfRange)?;
        let of_day = since.as_secs() % SECONDS_PER_DAY;
        let hour = of_day / SECONDS_PER_HOUR;
        let minute = of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE;
        let second = of_day % SECONDS_PER_MINUTE;
        let millis = since.subsec_millis();
        Ok(Self(format!(
            "{date}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
        )))
    }
}

impl From<FetchedAt> for String {
    fn from(at: FetchedAt) -> Self {
        at.0
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::FetchedAt;
    use crate::CalendarDateError;

    #[test]
    fn a_clock_reading_is_written_in_utc_to_the_millisecond() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_millis(1_791_883_800_123);

        let fetched = FetchedAt::from_system_time(time);

        assert_eq!(
            fetched.as_ref().map(FetchedAt::as_str),
            Ok("2026-10-13T09:30:00.123Z")
        );
    }

    #[test]
    fn the_epoch_and_the_last_millisecond_of_9999_are_written() {
        let last = SystemTime::UNIX_EPOCH + Duration::from_millis(253_402_300_799_999);

        let written = [SystemTime::UNIX_EPOCH, last]
            .map(|time| FetchedAt::from_system_time(time).map(|at| at.as_str().to_owned()));

        assert_eq!(
            written,
            [
                Ok("1970-01-01T00:00:00.000Z".to_owned()),
                Ok("9999-12-31T23:59:59.999Z".to_owned())
            ]
        );
    }

    #[test]
    fn a_reading_outside_the_calendar_is_refused() {
        let before = SystemTime::UNIX_EPOCH - Duration::from_millis(1);
        let after = SystemTime::UNIX_EPOCH + Duration::from_hours(70_389_528);

        let refused = [before, after].map(FetchedAt::from_system_time);

        assert_eq!(
            refused,
            [
                Err(CalendarDateError::OutOfRange),
                Err(CalendarDateError::OutOfRange)
            ]
        );
    }

    #[test]
    fn a_written_reading_reads_back() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_millis(951_782_400_007);

        let written = FetchedAt::from_system_time(time).map(|at| at.as_str().parse::<FetchedAt>());

        assert_eq!(
            written
                .as_ref()
                .map(|read| read.as_ref().map(FetchedAt::as_str)),
            Ok(Ok("2000-02-29T00:00:00.007Z"))
        );
    }
}
