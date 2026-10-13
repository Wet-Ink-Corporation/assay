//! The recorded expiry of the OAuth token and doctor's T-30 warning (05 §5, #93): the calendar
//! date it is recorded as, refused rather than repaired, and the warning's boundaries.

use std::error::Error;
use std::time::{Duration, SystemTime};

use assay_app::{
    CalendarDate, CalendarDateError, EXPIRY_WARNING_DAYS, ExpiryWarning, TOKEN_ROTATION_RUNBOOK,
    token_expiry_warning,
};
use proptest::prelude::*;

/// Seconds in a UTC day.
const DAY: u64 = 86_400;

/// Days from 1970-01-01 to 9999-12-31, the last date a `CalendarDate` holds.
const LAST_DAY: u64 = 2_932_896;

fn date(text: &str) -> Result<CalendarDate, CalendarDateError> {
    text.parse()
}

#[test]
fn the_epoch_is_day_zero() -> Result<(), Box<dyn Error>> {
    assert_eq!(date("1970-01-01")?.days_since_epoch(), 0, "the epoch");
    assert_eq!(date("1970-03-01")?.days_since_epoch(), 59, "past February");
    assert_eq!(
        date("2026-10-13")?.days_since_epoch(),
        20_739,
        "a known day"
    );
    Ok(())
}

#[test]
fn leap_days_are_accepted_only_in_leap_years() -> Result<(), Box<dyn Error>> {
    assert_eq!(date("2000-02-29")?.to_string(), "2000-02-29");
    assert_eq!(date("2028-02-29")?.to_string(), "2028-02-29");
    assert_eq!(date("2027-02-29"), Err(CalendarDateError::NoSuchDay));
    assert_eq!(date("2100-02-29"), Err(CalendarDateError::NoSuchDay));
    Ok(())
}

#[test]
fn a_month_or_day_outside_the_calendar_is_no_such_day() {
    for text in [
        "2027-00-10",
        "2027-13-10",
        "2027-01-00",
        "2027-01-32",
        "2027-04-31",
    ] {
        assert_eq!(date(text), Err(CalendarDateError::NoSuchDay), "{text}");
    }
}

#[test]
fn anything_but_ten_ascii_bytes_yyyy_mm_dd_is_refused_as_shape() {
    for text in [
        "",
        "2027-1-10",
        "2027-01-1",
        "2027-01-100",
        "02027-01-10",
        "2027/01/10",
        "2027-01-10 ",
        " 2027-01-10",
        "+027-01-10",
        "2027-0a-10",
        "2027-01-1\u{0663}",
        "2027-01-10T00:00:00Z",
    ] {
        assert_eq!(date(text), Err(CalendarDateError::Shape), "{text:?}");
    }
}

#[test]
fn dates_before_the_epoch_are_out_of_range_and_9999_12_31_is_the_last() -> Result<(), Box<dyn Error>>
{
    assert_eq!(date("1969-12-31"), Err(CalendarDateError::OutOfRange));
    assert_eq!(date("0001-01-01"), Err(CalendarDateError::OutOfRange));
    assert_eq!(
        u64::from(date("9999-12-31")?.days_since_epoch()),
        LAST_DAY,
        "the last day"
    );
    Ok(())
}

#[test]
fn an_error_never_echoes_the_refused_value() {
    let secret = "sk-ant-oat01-not-a-date";

    let shown = date(secret).map_or_else(|error| error.to_string(), |day| day.to_string());

    assert!(!shown.contains("sk-ant"), "the message was {shown:?}");
}

#[test]
fn system_time_is_read_as_its_utc_day() -> Result<(), Box<dyn Error>> {
    let at = |seconds| {
        CalendarDate::from_system_time(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
    };

    assert_eq!(at(0)?, date("1970-01-01")?);
    assert_eq!(at(DAY - 1)?, date("1970-01-01")?);
    assert_eq!(at(DAY)?, date("1970-01-02")?);
    assert_eq!(at(LAST_DAY * DAY + DAY - 1)?, date("9999-12-31")?);
    assert_eq!(at((LAST_DAY + 1) * DAY), Err(CalendarDateError::OutOfRange));
    Ok(())
}

#[test]
fn system_time_before_the_epoch_is_out_of_range() {
    let before = SystemTime::UNIX_EPOCH - Duration::from_secs(1);

    assert_eq!(
        CalendarDate::from_system_time(before),
        Err(CalendarDateError::OutOfRange)
    );
}

proptest! {
    #[test]
    fn every_date_reads_back_from_its_display(day in 0..=LAST_DAY) {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(day * DAY);
        let date = CalendarDate::from_system_time(at).map_err(|e| TestCaseError::fail(e.to_string()))?;

        let shown = date.to_string();

        prop_assert_eq!(shown.len(), 10);
        prop_assert_eq!(shown.parse::<CalendarDate>(), Ok(date));
        prop_assert_eq!(u64::from(date.days_since_epoch()), day);
    }
}

/// The warning for an expiry `days` days after 2026-10-13 (before it when negative).
fn warning_at(days: i64) -> Result<Option<ExpiryWarning>, Box<dyn Error>> {
    let today = date("2026-10-13")?;
    let seconds = i64::try_from(u64::from(today.days_since_epoch()) * DAY)? + days * 86_400;
    let expires_on = CalendarDate::from_system_time(
        SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds)?),
    )?;
    Ok(token_expiry_warning(expires_on, today))
}

#[test]
fn no_warning_more_than_thirty_days_out() -> Result<(), Box<dyn Error>> {
    assert_eq!(EXPIRY_WARNING_DAYS, 30, "05 §5: T-30 days");
    assert_eq!(warning_at(31)?, None);
    assert_eq!(warning_at(365)?, None);
    Ok(())
}

#[test]
fn warns_from_thirty_days_out_to_the_last_day() -> Result<(), Box<dyn Error>> {
    let expires_on = date("2026-11-12")?;

    assert_eq!(
        warning_at(30)?,
        Some(ExpiryWarning::ExpiresSoon {
            expires_on,
            days_left: 30
        })
    );
    assert_eq!(
        warning_at(1)?,
        Some(ExpiryWarning::ExpiresSoon {
            expires_on: date("2026-10-14")?,
            days_left: 1
        })
    );
    Ok(())
}

#[test]
fn the_token_is_expired_from_its_expiry_date_on() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        warning_at(0)?,
        Some(ExpiryWarning::Expired {
            expires_on: date("2026-10-13")?,
            days_ago: 0
        })
    );
    assert_eq!(
        warning_at(-1)?,
        Some(ExpiryWarning::Expired {
            expires_on: date("2026-10-12")?,
            days_ago: 1
        })
    );
    Ok(())
}

#[test]
fn only_an_expired_token_is_expired() -> Result<(), Box<dyn Error>> {
    let soon = warning_at(1)?.ok_or("a warning a day out")?;
    let expired = warning_at(0)?.ok_or("a warning on the day")?;

    assert!(!soon.is_expired(), "one day left");
    assert!(expired.is_expired(), "the expiry date");
    Ok(())
}

#[test]
fn every_warning_names_the_date_and_links_the_runbook() -> Result<(), Box<dyn Error>> {
    let soon = warning_at(30)?.ok_or("a warning at T-30")?.to_string();
    let expired = warning_at(-3)?.ok_or("a warning after expiry")?.to_string();

    assert_eq!(
        soon,
        format!(
            "the OAuth token expires on 2026-11-12, in 30 days; rotate it: {TOKEN_ROTATION_RUNBOOK}"
        )
    );
    assert_eq!(
        expired,
        format!(
            "the OAuth token expired on 2026-10-10, 3 days ago; rotate it: {TOKEN_ROTATION_RUNBOOK}"
        )
    );
    Ok(())
}
