//! Ledger timestamps and months: one canonical text form, read back exactly (06 §1).

use std::error::Error;
use std::time::Duration;

use assay_app::{LedgerTime, ParseLedgerTimeError};

#[test]
fn a_ledger_time_reads_and_writes_the_canonical_form() -> Result<(), Box<dyn Error>> {
    let text = "2026-10-31T23:59:59.999Z";

    let time: LedgerTime = text.parse()?;

    assert_eq!(time.to_string(), text);
    assert_eq!(time.unix_millis(), 1_793_491_199_999);
    Ok(())
}

#[test]
fn a_ledger_time_refuses_every_other_form() {
    for text in [
        "2026-10-31T23:59:59Z",
        "2026-10-31T23:59:59.99Z",
        "2026-10-31T23:59:59.9999Z",
        "2026-10-31t23:59:59.999Z",
        "2026-10-31T23:59:59.999+00:00",
        "2026-02-30T00:00:00.000Z",
        "2026-10-31T24:00:00.000Z",
        " 2026-10-31T23:59:59.999Z",
        "",
    ] {
        assert_eq!(
            text.parse::<LedgerTime>(),
            Err(ParseLedgerTimeError),
            "{text:?}"
        );
    }
}

#[test]
fn a_ledger_time_names_the_month_its_file_is_for() -> Result<(), Box<dyn Error>> {
    let time: LedgerTime = "2026-11-01T00:00:00.000Z".parse()?;

    let day_before = time.saturating_sub(Duration::from_hours(24));

    assert_eq!(time.month().to_string(), "2026-11");
    assert_eq!(day_before.month().to_string(), "2026-10");
    Ok(())
}

#[test]
fn a_ledger_time_before_the_epoch_saturates_at_the_epoch() {
    let epoch = LedgerTime::from_unix_millis(0).expect("the epoch is in range");

    assert_eq!(epoch.saturating_sub(Duration::from_hours(1)), epoch);
    assert_eq!(epoch.to_string(), "1970-01-01T00:00:00.000Z");
}

#[test]
fn a_ledger_time_after_year_9999_is_refused() {
    assert_eq!(
        LedgerTime::from_unix_millis(253_402_300_800_000),
        Err(ParseLedgerTimeError)
    );
    assert_eq!(
        LedgerTime::from_unix_millis(253_402_300_799_999).map(|time| time.to_string()),
        Ok("9999-12-31T23:59:59.999Z".to_owned())
    );
}
