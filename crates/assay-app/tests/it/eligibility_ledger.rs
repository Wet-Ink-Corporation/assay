//! `LedgerSnapshot`, the GitHub state snapshot's ledger port over the ledger's push and command
//! records (D15, #58): every record of the PR in append order, or `TooMany` past the cap.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    Command, CommandRecord, CommentId, LedgerRecords, LedgerSnapshot, LedgerSnapshotError,
    PushDraft, PushKind, PushRecord, Sha256Digest, SnapshotCommand, SnapshotLedger, StatePorts,
    StateRequest, UserId, append_command, append_push, github_state, read_pushes,
};

use crate::eligibility_fakes::{APPROVER, BASE, CONTRIBUTOR, FIRST, World, comment, sha, user};
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
                head_sha: commit('b'),
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
fn command_records_are_read_as_the_snapshot_judges_them() -> Result<(), Box<dyn Error>> {
    // D16 of #61: `assay.ledger-command/1` now records `head_sha` (D1), so a record is mapped,
    // its command re-parsed from its canonical line, where it was refused as `HeadUnrecorded`.
    let ledger = with_commands(2)?;

    let listed = snapshot(&ledger).command_records(id(7), 2)?;

    let dismissal = |comment_id| -> Result<SnapshotCommand, Box<dyn Error>> {
        Ok(SnapshotCommand {
            comment_id: CommentId(id(comment_id)),
            actor: UserId(id(1001)),
            created_at: "2026-10-12T09:29:58Z".parse()?,
            head_sha: commit('b'),
            body_sha256: digest('c'),
            command: Command::Dismiss {
                finding: "0123456789abcdef".parse()?,
                reason: "not a bug".to_owned(),
            },
        })
    };
    assert_eq!(
        listed,
        LedgerRecords::Listed(vec![dismissal(1)?, dismissal(2)?])
    );
    Ok(())
}

#[test]
fn github_state_applies_a_recorded_waive_of_this_head() -> Result<(), Box<dyn Error>> {
    let mut world = World::opened_by(CONTRIBUTOR);
    let body = "/assay waive 0123456789abcdef";
    let posted = comment(40, APPROVER, 5, body);
    let ledger = MemoryLedger::default();
    append_push(
        &ledger,
        &PushDraft {
            run_uid: run(1),
            pr: id(7),
            kind: PushKind::Opener,
            after: sha(FIRST),
            sender_id: user(CONTRIBUTOR),
            at: at("2026-10-13T09:00:00.000Z"),
        },
    )?;
    append_command(
        &ledger,
        &CommandRecord {
            run_uid: run(9),
            pr: id(7),
            comment_id: posted.id,
            actor_id: user(APPROVER),
            head_sha: sha(FIRST),
            command: body.to_owned(),
            body_sha256: Sha256Digest::of(body.as_bytes()),
            created_at: at("2026-10-13T09:05:00.000Z"),
            at: at("2026-10-13T09:05:02.000Z"),
        },
    )?;
    world.comments.insert(posted.id, posted);
    let base = sha(BASE);

    let state = github_state(
        &StatePorts {
            forge: &world,
            objects: &world,
            ledger: &snapshot(&ledger),
            comments: &world,
        },
        &StateRequest {
            number: world.number,
            author: world.author,
            base_sha: &base,
            head_sha: &world.head,
            lists: &world.lists,
        },
    )?;

    assert_eq!(
        state
            .waivers()
            .iter()
            .map(|waiver| (waiver.fp.to_string(), waiver.by.as_str()))
            .collect::<Vec<_>>(),
        [("0123456789abcdef".to_owned(), "approver")]
    );
    assert_eq!(state.voided().count(), 0);
    Ok(())
}

#[test]
fn an_unreadable_ledger_is_an_error() {
    let ledger = MemoryLedger::default().down();

    let listed = snapshot(&ledger).push_records(id(7), 2);

    assert_matches!(listed, Err(LedgerSnapshotError::Read(_)));
}
