//! When a ledger record was written, to the millisecond, and the month whose cost file holds it.
//!
//! One text form only, `YYYY-MM-DDTHH:MM:SS.mmmZ`: assay writes every ledger time, so a time in
//! any other form was not written by assay and is refused (wi-b5affa).

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use crate::calendar::CalendarDate;

/// Milliseconds in a second, a minute, an hour and a UTC day (no leap seconds).
const MILLIS_PER_SECOND: u64 = 1_000;
const MILLIS_PER_MINUTE: u64 = 60_000;
const MILLIS_PER_HOUR: u64 = 3_600_000;
const MILLIS_PER_DAY: u64 = 86_400_000;

/// The last millisecond of 9999-12-31, the last day a [`CalendarDate`] holds.
const LAST_MILLI: u64 = 253_402_300_799_999;

/// A UTC instant from 1970-01-01 to 9999-12-31, to the millisecond: a ledger record's `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LedgerTime {
    millis: u64,
}

/// A text or clock reading that is not a [`LedgerTime`]. The refused text is never shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the time is not a UTC date-time written YYYY-MM-DDTHH:MM:SS.mmmZ from 1970 to 9999")]
pub struct ParseLedgerTimeError;

impl LedgerTime {
    /// The instant `millis` milliseconds after the Unix epoch.
    ///
    /// # Errors
    ///
    /// [`ParseLedgerTimeError`] after 9999-12-31.
    pub const fn from_unix_millis(millis: u64) -> Result<Self, ParseLedgerTimeError> {
        if millis > LAST_MILLI {
            return Err(ParseLedgerTimeError);
        }
        Ok(Self { millis })
    }

    /// `time`, truncated to the millisecond: how a job stamps the records it writes.
    ///
    /// # Errors
    ///
    /// [`ParseLedgerTimeError`] before the Unix epoch or after 9999-12-31.
    pub fn from_system_time(time: SystemTime) -> Result<Self, ParseLedgerTimeError> {
        let since = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_before| ParseLedgerTimeError)?;
        let millis = u64::try_from(since.as_millis()).map_err(|_far| ParseLedgerTimeError)?;
        Self::from_unix_millis(millis)
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub const fn unix_millis(self) -> u64 {
        self.millis
    }

    /// The instant `duration` earlier, or the epoch when that is before it.
    #[must_use]
    pub fn saturating_sub(self, duration: Duration) -> Self {
        let back = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        Self {
            millis: self.millis.saturating_sub(back),
        }
    }

    /// The UTC month this instant falls in: the cost file `ledger/YYYY-MM.jsonl` it belongs to.
    #[must_use]
    pub fn month(self) -> LedgerMonth {
        let (year, month) = self.date().year_month();
        LedgerMonth { year, month }
    }

    /// The UTC day this instant falls in. Every `LedgerTime` is at most 9999-12-31, so the
    /// count never saturates.
    fn date(self) -> CalendarDate {
        let days = u32::try_from(self.millis / MILLIS_PER_DAY).unwrap_or(u32::MAX);
        CalendarDate::from_days_saturating(days)
    }
}

impl FromStr for LedgerTime {
    type Err = ParseLedgerTimeError;

    /// Reads exactly `YYYY-MM-DDTHH:MM:SS.mmmZ` naming a real day and time.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (Some(date), Some(clock)) = (text.get(..10), text.get(10..)) else {
            return Err(ParseLedgerTimeError);
        };
        let &[
            b'T',
            h0,
            h1,
            b':',
            n0,
            n1,
            b':',
            s0,
            s1,
            b'.',
            m0,
            m1,
            m2,
            b'Z',
        ] = clock.as_bytes()
        else {
            return Err(ParseLedgerTimeError);
        };
        let date: CalendarDate = date.parse().map_err(|_date| ParseLedgerTimeError)?;
        let hour = digits(&[h0, h1]).filter(|&hour| hour < 24);
        let minute = digits(&[n0, n1]).filter(|&minute| minute < 60);
        let second = digits(&[s0, s1]).filter(|&second| second < 60);
        let (Some(hour), Some(minute), Some(second), Some(milli)) =
            (hour, minute, second, digits(&[m0, m1, m2]))
        else {
            return Err(ParseLedgerTimeError);
        };
        let millis = u64::from(date.days_since_epoch()) * MILLIS_PER_DAY
            + hour * MILLIS_PER_HOUR
            + minute * MILLIS_PER_MINUTE
            + second * MILLIS_PER_SECOND
            + milli;
        Self::from_unix_millis(millis)
    }
}

/// The value of `bytes` when all are ASCII digits.
fn digits(bytes: &[u8]) -> Option<u64> {
    bytes.iter().try_fold(0_u64, |value, &byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u64::from(byte - b'0'))
    })
}

impl fmt::Display for LedgerTime {
    /// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let of_day = self.millis % MILLIS_PER_DAY;
        let hour = of_day / MILLIS_PER_HOUR;
        let minute = of_day % MILLIS_PER_HOUR / MILLIS_PER_MINUTE;
        let second = of_day % MILLIS_PER_MINUTE / MILLIS_PER_SECOND;
        let milli = of_day % MILLIS_PER_SECOND;
        write!(
            f,
            "{}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z",
            self.date()
        )
    }
}

/// A UTC month: the cost file `ledger/YYYY-MM.jsonl` (06 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LedgerMonth {
    year: u32,
    month: u32,
}

impl fmt::Display for LedgerMonth {
    /// `YYYY-MM`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}
