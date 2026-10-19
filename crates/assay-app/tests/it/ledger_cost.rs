//! Cost records: appended to `ledger/YYYY-MM.jsonl` by compare-and-set, deduplicated by
//! `run_uid` and kind, and refused whole when the file breaks a rule (06 §1, R3-16, T23).

use std::error::Error;

use assay_app::{
    Appended, Corruption, CostKind, LedgerPath, LedgerReadError, LedgerWriteError,
    MAX_CAS_ATTEMPTS, MAX_LEDGER_FILE_BYTES, WriteCondition, WriteOutcome, append_cost,
};

use crate::ledger_fakes::{MemoryLedger, WriteCall, at, cost, cost_line};

fn october() -> LedgerPath {
    LedgerPath::costs(at("2026-10-12T09:30:00.000Z").month())
}

#[test]
fn the_first_cost_record_of_a_month_creates_its_file() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();
    let record = cost(1, CostKind::Reserved, 740_000, "2026-10-12T09:30:00.000Z");

    let appended = append_cost(&ledger, &record)?;

    assert_eq!(appended, Appended::Recorded);
    assert_eq!(october().as_str(), "ledger/2026-10.jsonl");
    assert_eq!(
        ledger.text(&october()),
        cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z")
    );
    assert_eq!(
        ledger.writes(),
        [WriteCall {
            path: "ledger/2026-10.jsonl".to_owned(),
            condition: WriteCondition::Absent,
            outcome: WriteOutcome::Written,
        }]
    );
    Ok(())
}

#[test]
fn a_cost_record_is_appended_after_the_records_already_there() -> Result<(), Box<dyn Error>> {
    let first = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), first.as_bytes());
    let record = cost(1, CostKind::Final, 512_000, "2026-10-12T09:41:00.000Z");

    let appended = append_cost(&ledger, &record)?;

    assert_eq!(appended, Appended::Recorded);
    assert_eq!(
        ledger.text(&october()),
        first + &cost_line(1, "final", "0.512", "2026-10-12T09:41:00.000Z")
    );
    Ok(())
}

#[test]
fn a_cost_record_already_present_is_not_written_again() -> Result<(), Box<dyn Error>> {
    let first = cost_line(1, "final", "0.74", "2026-10-12T09:30:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), first.as_bytes());
    let backfill = cost(1, CostKind::Final, 2_300_000, "2026-10-13T00:00:00.000Z");

    let appended = append_cost(&ledger, &backfill)?;

    assert_eq!(
        appended,
        Appended::AlreadyRecorded(cost(
            1,
            CostKind::Final,
            740_000,
            "2026-10-12T09:30:00.000Z"
        ))
    );
    assert_eq!(ledger.text(&october()), first);
    assert_eq!(ledger.writes(), []);
    Ok(())
}

#[test]
fn a_backfill_after_a_month_boundary_finds_the_final_record_of_the_month_before()
-> Result<(), Box<dyn Error>> {
    let settled = cost_line(1, "final", "0.5", "2026-10-31T23:59:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), settled.as_bytes());
    let backfill = cost(1, CostKind::Final, 2_300_000, "2026-11-01T00:30:00.000Z");

    let appended = append_cost(&ledger, &backfill)?;

    assert_eq!(
        appended,
        Appended::AlreadyRecorded(cost(
            1,
            CostKind::Final,
            500_000,
            "2026-10-31T23:59:00.000Z"
        ))
    );
    assert_eq!(ledger.writes(), []);
    Ok(())
}

#[test]
fn a_concurrent_append_is_retried_on_the_fresh_file_and_both_records_survive()
-> Result<(), Box<dyn Error>> {
    let rival = cost_line(2, "reserved", "2.3", "2026-10-12T09:29:00.000Z");
    let ledger = MemoryLedger::default().rival_appends(rival.as_bytes());
    let record = cost(1, CostKind::Reserved, 740_000, "2026-10-12T09:30:00.000Z");

    let appended = append_cost(&ledger, &record)?;

    assert_eq!(appended, Appended::Recorded);
    assert_eq!(
        ledger.text(&october()),
        rival + &cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z")
    );
    let outcomes: Vec<_> = ledger.writes().into_iter().map(|w| w.outcome).collect();
    assert_eq!(outcomes, [WriteOutcome::Conflict, WriteOutcome::Written]);
    Ok(())
}

#[test]
fn an_append_that_keeps_conflicting_is_contended_after_the_last_attempt() {
    let ledger = MemoryLedger::default().rival_always();
    let record = cost(1, CostKind::Reserved, 740_000, "2026-10-12T09:30:00.000Z");

    let appended = append_cost(&ledger, &record);

    assert!(
        matches!(
            &appended,
            Err(LedgerWriteError::Contended { path, attempts })
                if *path == october() && *attempts == MAX_CAS_ATTEMPTS
        ),
        "{appended:?}"
    );
    assert_eq!(ledger.writes().len(), usize::from(MAX_CAS_ATTEMPTS));
}

#[test]
fn an_unreachable_ledger_is_a_read_error_and_nothing_is_written() {
    let ledger = MemoryLedger::default().down();
    let record = cost(1, CostKind::Reserved, 740_000, "2026-10-12T09:30:00.000Z");

    let appended = append_cost(&ledger, &record);

    assert!(
        matches!(
            appended,
            Err(LedgerWriteError::Read(LedgerReadError::Ledger(_)))
        ),
        "{appended:?}"
    );
}

/// Appending to a cost file holding `contents` is refused at `line` for `problem`, with nothing
/// written.
fn refused(contents: &str, line: usize, problem: Corruption) {
    let ledger = MemoryLedger::default().with_file(&october(), contents.as_bytes());
    let record = cost(9, CostKind::Reserved, 1, "2026-10-12T09:30:00.000Z");

    let appended = append_cost(&ledger, &record);

    assert!(
        matches!(
            &appended,
            Err(LedgerWriteError::Read(LedgerReadError::Corrupt { path, line: at, problem: found }))
                if *path == october() && *at == line && *found == problem
        ),
        "{contents:?}: {appended:?}"
    );
    assert_eq!(ledger.writes(), [], "a refused file is never written");
}

#[test]
fn a_cost_file_whose_last_line_is_unterminated_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    refused(line.trim_end(), 1, Corruption::Unterminated);
}

#[test]
fn a_cost_file_with_an_empty_line_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    refused(&format!("{line}\n"), 2, Corruption::EmptyLine);
}

#[test]
fn a_cost_line_over_the_record_cap_is_refused() {
    refused(&format!("{}\n", " ".repeat(5_000)), 1, Corruption::LongLine);
}

#[test]
fn a_cost_line_that_is_not_exactly_one_object_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    let record = line.trim_end();
    refused(&format!(" {record}\n"), 1, Corruption::Shape);
    refused(&format!("{record} \n"), 1, Corruption::Shape);
}

#[test]
fn a_cost_line_that_repeats_a_key_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    let repeated = line.replacen(r#""pr":7,"#, r#""pr":7,"pr":8,"#, 1);
    refused(&repeated, 1, Corruption::Shape);
}

#[test]
fn a_cost_line_with_an_unknown_field_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    let extra = line.replacen(r#""pr":7,"#, r#""pr":7,"note":"x","#, 1);
    refused(&extra, 1, Corruption::Shape);
}

#[test]
fn a_cost_line_of_another_contract_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    refused(
        &line.replacen("assay.ledger-cost/1", "assay.ledger-cost/2", 1),
        1,
        Corruption::Schema,
    );
}

#[test]
fn a_cost_line_with_an_inexact_amount_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    let inexact = line.replacen(r#""aeu":"0.74","kind""#, r#""aeu":"0.740","kind""#, 1);
    refused(&inexact, 1, Corruption::Field("aeu"));
}

#[test]
fn a_cost_line_that_repeats_a_role_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    let role = r#"{"role":"combined","aeu":"0.74"}"#;
    let twice = line.replacen(role, &format!("{role},{role}"), 1);
    refused(&twice, 1, Corruption::Field("roles"));
}

#[test]
fn a_cost_line_with_a_short_head_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    refused(
        &line.replacen("bbbbbbbbbb", "", 1),
        1,
        Corruption::Field("head_sha"),
    );
}

#[test]
fn a_cancelled_reservation_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    refused(
        &line.replacen(r#""cancelled":false"#, r#""cancelled":true"#, 1),
        1,
        Corruption::Field("cancelled"),
    );
}

#[test]
fn a_cost_record_of_another_month_is_refused() {
    let line = cost_line(1, "reserved", "0.74", "2026-09-30T23:59:59.999Z");
    refused(&line, 1, Corruption::WrongMonth);
}

#[test]
fn a_cost_file_that_repeats_a_run_and_kind_is_refused() {
    let first = cost_line(1, "reserved", "0.74", "2026-10-12T09:30:00.000Z");
    let again = cost_line(1, "reserved", "0.5", "2026-10-12T09:31:00.000Z");
    refused(&(first + &again), 2, Corruption::Duplicate);
}

#[test]
fn a_record_its_own_reader_would_refuse_is_never_written() {
    let ledger = MemoryLedger::default();
    let mut record = cost(1, CostKind::Reserved, 740_000, "2026-10-12T09:30:00.000Z");
    record.repo = "not a repository".to_owned();

    let appended = append_cost(&ledger, &record);

    assert!(
        matches!(
            &appended,
            Err(LedgerWriteError::Invalid {
                problem: Corruption::Field("repo"),
                ..
            })
        ),
        "{appended:?}"
    );
    assert_eq!(ledger.writes(), []);
}

#[test]
fn a_cost_file_over_the_cap_is_refused_unparsed() {
    let ledger =
        MemoryLedger::default().with_file(&october(), &vec![b'x'; MAX_LEDGER_FILE_BYTES + 1]);
    let record = cost(1, CostKind::Reserved, 740_000, "2026-10-12T09:30:00.000Z");

    let appended = append_cost(&ledger, &record);

    assert!(
        matches!(
            &appended,
            Err(LedgerWriteError::Read(LedgerReadError::TooLarge { size, .. }))
                if *size == MAX_LEDGER_FILE_BYTES + 1
        ),
        "{appended:?}"
    );
}

#[test]
fn an_append_that_would_pass_the_cap_is_full_and_writes_nothing() {
    let when = "2026-10-12T09:30:00.000Z";
    let mut contents = String::new();
    let mut run = 1_u64;
    loop {
        let line = cost_line(run, "reserved", "0.74", when);
        if contents.len() + line.len() > MAX_LEDGER_FILE_BYTES {
            break;
        }
        contents.push_str(&line);
        run += 1;
    }
    let ledger = MemoryLedger::default().with_file(&october(), contents.as_bytes());
    let record = cost(run, CostKind::Reserved, 740_000, when);

    let appended = append_cost(&ledger, &record);

    assert!(
        matches!(&appended, Err(LedgerWriteError::Full { path, .. }) if *path == october()),
        "{appended:?}"
    );
    assert_eq!(ledger.writes(), []);
}
