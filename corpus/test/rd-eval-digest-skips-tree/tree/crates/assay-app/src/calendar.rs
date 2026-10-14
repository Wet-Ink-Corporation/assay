//! A UTC calendar day, the unit the OAuth token's expiry is recorded in (05 §5, #93).
//!
//! Only std: a day is a count from 1970-01-01, converted to and from the proleptic Gregorian
//! calendar with Howard Hinnant's `days_from_civil` and `civil_from_days`. The range is
//! 1970-01-01 to 9999-12-31, so every count fits a `u32` and no step can overflow.

use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use thiserror::Error;

/// Seconds in a UTC day (no leap seconds, as Unix time counts).
const SECONDS_PER_DAY: u64 = 86_400;

/// The last year a [`CalendarDate`] holds.
const LAST_YEAR: u32 = 9999;

/// The first year a [`CalendarDate`] holds.
const FIRST_YEAR: u32 = 1970;

/// Days from 0000-03-01 to 1970-01-01, the shift between Hinnant's era count and Unix days.
const EPOCH_SHIFT: u32 = 719_468;

/// Days in a 400-year era.
const DAYS_PER_ERA: u32 = 146_097;

/// Days from 1970-01-01 to 9999-12-31.
const LAST_DAY: u32 = 2_932_896;

/// A day of the proleptic Gregorian calendar, from 1970-01-01 to 9999-12-31.
///
/// Read with [`FromStr`] from exactly `YYYY-MM-DD` and shown the same way; read from the clock
/// with [`CalendarDate::from_system_time`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CalendarDate {
    days: u32,
}

/// Why no [`CalendarDate`] could be read. The refused value is never shown: a date field that
/// holds something else may hold a secret pasted into the wrong place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CalendarDateError {
    /// The text is not exactly ten ASCII bytes `YYYY-MM-DD`.
    #[error("the date is not written YYYY-MM-DD")]
    Shape,
    /// The month or the day does not exist in that year.
    #[error("the date names no day of the calendar")]
    NoSuchDay,
    /// The date or time is before 1970-01-01 or after 9999-12-31.
    #[error("the date is outside 1970-01-01 to 9999-12-31")]
    OutOfRange,
}

impl CalendarDate {
    /// The UTC day `time` falls on.
    ///
    /// # Errors
    /// [`CalendarDateError::OutOfRange`] when `time` is before the Unix epoch or after
    /// 9999-12-31.
    pub fn from_system_time(time: SystemTime) -> Result<Self, CalendarDateError> {
        let since = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_before| CalendarDateError::OutOfRange)?;
        let days = u32::try_from(since.as_secs() / SECONDS_PER_DAY)
            .map_err(|_too_far| CalendarDateError::OutOfRange)?;
        if days > LAST_DAY {
            return Err(CalendarDateError::OutOfRange);
        }
        Ok(Self { days })
    }

    /// Days since 1970-01-01.
    #[must_use]
    pub const fn days_since_epoch(self) -> u32 {
        self.days
    }

    /// The year and the month (1 to 12) the day falls in.
    pub(crate) const fn year_month(self) -> (u32, u32) {
        let (year, month, _day) = civil_from_days(self.days);
        (year, month)
    }
}

/// Whether `year` has a 29 February.
const fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// Days in `month` (1 to 12) of `year`; 0 for a month that does not exist.
const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 of a valid date from 1970 to 9999 (Hinnant's `days_from_civil`).
const fn days_from_civil(year: u32, month: u32, day: u32) -> u32 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let march_month = (month + 9) % 12;
    let day_of_year = (153 * march_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * DAYS_PER_ERA + day_of_era - EPOCH_SHIFT
}

/// Year, month and day of a count from 1970-01-01 (Hinnant's `civil_from_days`).
const fn civil_from_days(days: u32) -> (u32, u32, u32) {
    let shifted = days + EPOCH_SHIFT;
    let era = shifted / DAYS_PER_ERA;
    let day_of_era = shifted - era * DAYS_PER_ERA;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    let year = year_of_era + era * 400;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// The value of `digits`, all ASCII digits, or `None`.
fn decimal(digits: &[u8]) -> Option<u32> {
    digits.iter().try_fold(0_u32, |value, &byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u32::from(byte - b'0'))
    })
}

impl FromStr for CalendarDate {
    type Err = CalendarDateError;

    /// Reads exactly `YYYY-MM-DD`: no sign, no whitespace, no time, no other separator.
    fn from_str(text: &str) -> Result<Self, CalendarDateError> {
        let Ok(&[y0, y1, y2, y3, b'-', m0, m1, b'-', d0, d1]) =
            <&[u8; 10]>::try_from(text.as_bytes())
        else {
            return Err(CalendarDateError::Shape);
        };
        let field = |digits: &[u8]| decimal(digits).ok_or(CalendarDateError::Shape);
        let year = field(&[y0, y1, y2, y3])?;
        let month = field(&[m0, m1])?;
        let day = field(&[d0, d1])?;
        if day == 0 || day > days_in_month(year, month) {
            return Err(CalendarDateError::NoSuchDay);
        }
        if !(FIRST_YEAR..=LAST_YEAR).contains(&year) {
            return Err(CalendarDateError::OutOfRange);
        }
        Ok(Self {
            days: days_from_civil(year, month, day),
        })
    }
}

impl fmt::Display for CalendarDate {
    /// `YYYY-MM-DD`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.days);
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}
