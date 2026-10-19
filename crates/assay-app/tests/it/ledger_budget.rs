//! The budget read: the last 24 hours of cost records, across a month boundary, with a `final`
//! record replacing its `reserved` one, plus live or unreaped leases; and the per-PR cooldown's
//! admissions (06 §1, R3-06, R3-27).

use std::error::Error;

use assay_app::{Corruption, CostKind, LeaseReservation, LedgerPath, LedgerReadError, read_costs};
use assay_domain::Aeu;

use crate::ledger_fakes::{MemoryLedger, at, cost, cost_line, id, run};

fn month_of(time: &str) -> LedgerPath {
    LedgerPath::costs(at(time).month())
}

#[test]
fn the_day_s_spend_reads_both_months_across_a_month_boundary() -> Result<(), Box<dyn Error>> {
    let october = cost_line(1, "reserved", "2.3", "2026-10-31T23:30:00.000Z");
    let november = cost_line(2, "final", "0.5", "2026-11-01T00:10:00.000Z");
    let ledger = MemoryLedger::default()
        .with_file(&month_of("2026-10-31T23:30:00.000Z"), october.as_bytes())
        .with_file(&month_of("2026-11-01T00:10:00.000Z"), november.as_bytes());

    let window = read_costs(&ledger, at("2026-11-01T12:00:00.000Z"))?;

    assert_eq!(window.day_spent(&[]), Aeu::from_micros(2_800_000));
    Ok(())
}

#[test]
fn a_window_inside_one_month_reads_only_that_month() -> Result<(), Box<dyn Error>> {
    let november = cost_line(2, "final", "0.5", "2026-11-02T00:10:00.000Z");
    let ledger = MemoryLedger::default()
        .with_file(&month_of("2026-10-31T23:30:00.000Z"), b"not a record")
        .with_file(&month_of("2026-11-02T00:10:00.000Z"), november.as_bytes());

    let window = read_costs(&ledger, at("2026-11-02T12:00:00.000Z"))?;

    assert_eq!(window.day_spent(&[]), Aeu::from_micros(500_000));
    Ok(())
}

#[test]
fn a_final_record_replaces_its_reservation_even_in_the_next_month() -> Result<(), Box<dyn Error>> {
    let october = cost_line(1, "reserved", "2.3", "2026-10-31T23:50:00.000Z");
    let november = cost_line(1, "final", "0.5", "2026-11-01T00:20:00.000Z");
    let ledger = MemoryLedger::default()
        .with_file(&month_of("2026-10-31T23:50:00.000Z"), october.as_bytes())
        .with_file(&month_of("2026-11-01T00:20:00.000Z"), november.as_bytes());

    let window = read_costs(&ledger, at("2026-11-01T10:00:00.000Z"))?;

    assert_eq!(window.day_spent(&[]), Aeu::from_micros(500_000));
    Ok(())
}

#[test]
fn a_reservation_counts_until_its_final_record_exists() -> Result<(), Box<dyn Error>> {
    let reserved = cost_line(1, "reserved", "2.3", "2026-10-12T09:30:00.000Z");
    let ledger = MemoryLedger::default()
        .with_file(&month_of("2026-10-12T09:30:00.000Z"), reserved.as_bytes());

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"))?;

    assert_eq!(window.day_spent(&[]), Aeu::from_micros(2_300_000));
    Ok(())
}

#[test]
fn a_record_counts_from_exactly_24_hours_ago_and_not_before() -> Result<(), Box<dyn Error>> {
    let edge = cost_line(1, "final", "1", "2026-10-11T10:00:00.000Z");
    let older = cost_line(2, "final", "2", "2026-10-11T09:59:59.999Z");
    let later = cost_line(3, "final", "4", "2026-10-12T10:00:00.001Z");
    let ledger = MemoryLedger::default().with_file(
        &month_of("2026-10-11T10:00:00.000Z"),
        (edge + &older + &later).as_bytes(),
    );

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"))?;

    assert_eq!(window.day_spent(&[]), Aeu::from_micros(5_000_000));
    Ok(())
}

#[test]
fn a_lease_counts_unless_its_run_is_already_counted() -> Result<(), Box<dyn Error>> {
    let reserved = cost_line(1, "reserved", "2.3", "2026-10-12T09:30:00.000Z");
    let ledger = MemoryLedger::default()
        .with_file(&month_of("2026-10-12T09:30:00.000Z"), reserved.as_bytes());
    let leases = [
        LeaseReservation {
            run_uid: run(1),
            reserved_aeu: Aeu::from_micros(2_300_000),
        },
        LeaseReservation {
            run_uid: run(3),
            reserved_aeu: Aeu::from_micros(740_000),
        },
    ];

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"))?;

    assert_eq!(window.day_spent(&leases), Aeu::from_micros(3_040_000));
    Ok(())
}

#[test]
fn a_lease_counts_when_its_run_s_record_is_before_the_window() -> Result<(), Box<dyn Error>> {
    let before = cost_line(1, "reserved", "2.3", "2026-10-11T09:59:59.999Z");
    let ledger =
        MemoryLedger::default().with_file(&month_of("2026-10-11T09:59:59.999Z"), before.as_bytes());
    let leases = [LeaseReservation {
        run_uid: run(1),
        reserved_aeu: Aeu::from_micros(1_500_000),
    }];

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"))?;

    assert_eq!(window.day_spent(&leases), Aeu::from_micros(1_500_000));
    Ok(())
}

#[test]
fn admissions_are_the_pr_s_reservations_within_the_hour() -> Result<(), Box<dyn Error>> {
    let mut other_pr = cost(4, CostKind::Reserved, 1, "2026-10-12T09:50:00.000Z");
    other_pr.pr = id(8);
    let lines = [
        cost_line(1, "reserved", "0.74", "2026-10-12T09:00:00.000Z"),
        cost_line(1, "final", "0.5", "2026-10-12T09:10:00.000Z"),
        cost_line(2, "reserved", "0.74", "2026-10-12T08:59:59.999Z"),
        cost_line(3, "reserved", "0.74", "2026-10-12T09:59:00.000Z"),
    ]
    .concat();
    let path = month_of("2026-10-12T09:00:00.000Z");
    let ledger = MemoryLedger::default().with_file(&path, lines.as_bytes());
    assay_app::append_cost(&ledger, &other_pr)?;

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"))?;

    assert_eq!(window.admissions(id(42), id(7)), 2);
    assert_eq!(window.admissions(id(42), id(8)), 1);
    assert_eq!(window.admissions(id(43), id(7)), 0);
    Ok(())
}

#[test]
fn a_corrupt_cost_file_fails_the_budget_read() {
    let path = month_of("2026-10-12T09:30:00.000Z");
    let ledger = MemoryLedger::default().with_file(&path, b"{}\n");

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"));

    assert!(
        matches!(
            &window,
            Err(LedgerReadError::Corrupt { path: found, line: 1, problem: Corruption::Shape })
                if *found == path
        ),
        "{window:?}"
    );
}

#[test]
fn an_unreachable_ledger_fails_the_budget_read() {
    let ledger = MemoryLedger::default().down();

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"));

    assert!(
        matches!(window, Err(LedgerReadError::Ledger(_))),
        "{window:?}"
    );
}

#[test]
fn an_absent_month_file_holds_no_spend() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    let window = read_costs(&ledger, at("2026-10-12T10:00:00.000Z"))?;

    assert_eq!(window.day_spent(&[]), Aeu::ZERO);
    assert_eq!(window.admissions(id(42), id(7)), 0);
    Ok(())
}
