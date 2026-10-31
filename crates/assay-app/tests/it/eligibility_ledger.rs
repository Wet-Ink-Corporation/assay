//! `LedgerSnapshot`, the GitHub state snapshot's ledger port over the ledger's push and command
//! records (D15, #58): every record of the PR in append order, or `TooMany` past the cap.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    CommandRecord, CommentId, LedgerRecords, LedgerSnapshot, LedgerSnapshotError, PushDraft,
    PushKind, PushRecord, SnapshotLedger, UserId, append_command, append_push, read_pushes,
};

use crate::ledger_fakes::{MemoryLedger, at, commit, digest, id, run};

/// Repository 42's snapshot ledger over `ledger`.
fn snapshot(ledger: &MemoryLedger) -> LedgerSnapshot<'_, MemoryLedger> {
    LedgerSnapshot {
        ledger,
        repo_id: id(42),
    }
}

/// A ledger holding `pushes` push records of PR 7, each by its own run.
fn with_pushes(pushes: u64) -> Result<MemoryLedger, Box<dyn Error>> {
    let ledger = MemoryLedger::default();
    for run_id in 1..=pushes {
        append_push(
            &ledger,
            &PushDraft {
                run_uid: run(run_id),
                pr: id(7),
                kind: PushKind::Push {
                    before: commit('a'),
                    forced: false,
                },
                after: commit('b'),
                sender_id: UserId(id(1001)),
                at: at("2026-10-12T09:30:00.000Z"),
            },
        )?;
    }
    Ok(ledger)
}

/// PR 7's push records in `ledger`, as the ledger reads them.
fn pushes(ledger: &MemoryLedger) -> Result<Vec<PushRecord>, Box<dyn Error>> {
    Ok(read_pushes(ledger, id(42), id(7))?)
}

/// A ledger holding `count` command records of PR 7, `/assay dismiss` each.
fn with_commands(count: u64) -> Result<MemoryLedger, Box<dyn Error>> {
    let ledger = MemoryLedger::default();
    for comment_id in 1..=count {
        append_command(
            &ledger,
            &CommandRecord {
                run_uid: run(9),
                pr: id(7),
                comment_id: CommentId(id(comment_id)),
                actor_id: UserId(id(1001)),
                command: "/assay dismiss 0123456789abcdef not a bug".to_owned(),
                body_sha256: digest('c'),
                created_at: at("2026-10-12T09:29:58.000Z"),
                at: at("2026-10-12T09:30:00.000Z"),
            },
        )?;
    }
    Ok(ledger)
}

#[test]
fn push_records_up_to_the_cap_are_listed_in_append_order() -> Result<(), Box<dyn Error>> {
    let ledger = with_pushes(3)?;

    let listed = snapshot(&ledger).push_records(id(7), 3)?;

    assert_eq!(listed, LedgerRecords::Listed(pushes(&ledger)?));
    Ok(())
}

#[test]
fn push_records_past_the_cap_are_too_many() -> Result<(), Box<dyn Error>> {
    let ledger = with_pushes(3)?;

    let listed = snapshot(&ledger).push_records(id(7), 2)?;

    assert_eq!(listed, LedgerRecords::TooMany);
    Ok(())
}

#[test]
fn a_pr_without_records_lists_none() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    let listed = snapshot(&ledger);

    assert_eq!(
        listed.push_records(id(7), 0)?,
        LedgerRecords::Listed(Vec::new())
    );
    assert_eq!(
        listed.command_records(id(7), 0)?,
        LedgerRecords::Listed(Vec::new())
    );
    Ok(())
}

#[test]
fn command_records_past_the_cap_are_too_many() -> Result<(), Box<dyn Error>> {
    let ledger = with_commands(3)?;

    let listed = snapshot(&ledger).command_records(id(7), 2)?;

    assert_eq!(listed, LedgerRecords::TooMany);
    Ok(())
}

#[test]
fn a_command_record_is_refused_because_it_does_not_record_its_head() -> Result<(), Box<dyn Error>> {
    // `assay.ledger-command/1` has no `head_sha`, which the snapshot judges head-bound commands
    // by: refused rather than guessed (wi-b5affa) until the contract records it.
    let ledger = with_commands(2)?;

    let listed = snapshot(&ledger).command_records(id(7), 2);

    assert_matches!(
        listed,
        Err(LedgerSnapshotError::HeadUnrecorded { comment_id }) if comment_id == CommentId(id(1))
    );
    Ok(())
}

#[test]
fn an_unreadable_ledger_is_an_error() {
    let ledger = MemoryLedger::default().down();

    let listed = snapshot(&ledger).push_records(id(7), 2);

    assert_matches!(listed, Err(LedgerSnapshotError::Read(_)));
}
