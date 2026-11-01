//! A judged command and its ledger record (`assay.ledger-command/1`, D1 of #61):
//! `SnapshotCommand::record` writes the command as its canonical line with its head, and
//! `TryFrom<&CommandRecord>` reads it back, refusing a line the snapshot cannot judge.

use std::num::NonZeroU64;

use assay_app::{
    CommandRecord, CommandTimeError, LedgerTime, Recorded, SnapshotCommand, UnreadCommand,
};
use assay_domain::RunUid;

use crate::eligibility_fakes::{APPROVER, CONTRIBUTOR, FIRST, World, comment, created, sha};

type Outcome = Result<(), Box<dyn std::error::Error>>;

/// Decide-only run 9 of repository 42.
fn decide_only_run() -> RunUid {
    RunUid {
        repo_id: NonZeroU64::new(42).expect("forty-two is not zero"),
        run_id: NonZeroU64::new(9).expect("nine is not zero"),
        run_attempt: NonZeroU64::MIN,
    }
}

/// A ledger time from its canonical text.
fn ledger_time(text: &str) -> LedgerTime {
    text.parse().expect("a canonical ledger time")
}

/// The approver's waive on a contributor's PR, posted at minute 5, as the snapshot records it.
fn approvers_waive() -> Result<SnapshotCommand, Box<dyn std::error::Error>> {
    let state = World::opened_by(CONTRIBUTOR).state()?;
    let posted = comment(40, APPROVER, 5, "/assay waive 0123456789abcdef");
    match state.record_command(&created(posted))? {
        Recorded::Record(record) => Ok(record),
        Recorded::Recheck => Err("a waive is recorded".into()),
    }
}

#[test]
fn a_judged_command_is_recorded_with_its_head_and_canonical_line() -> Outcome {
    let waive = approvers_waive()?;
    let pr = NonZeroU64::new(7).ok_or("seven")?;

    let record = waive.record(
        decide_only_run(),
        pr,
        ledger_time("2026-10-13T09:05:02.000Z"),
    )?;

    assert_eq!(
        record,
        CommandRecord {
            run_uid: decide_only_run(),
            pr,
            comment_id: waive.comment_id,
            actor_id: waive.actor,
            head_sha: sha(FIRST),
            command: "/assay waive 0123456789abcdef".to_owned(),
            body_sha256: waive.body_sha256.clone(),
            created_at: ledger_time("2026-10-13T09:05:00.000Z"),
            at: ledger_time("2026-10-13T09:05:02.000Z"),
        }
    );
    assert_eq!(SnapshotCommand::try_from(&record), Ok(waive));
    Ok(())
}

#[test]
fn a_comment_time_finer_than_a_millisecond_is_never_recorded() -> Outcome {
    let mut waive = approvers_waive()?;
    waive.created_at = "2026-10-13T09:05:00.0005Z".parse()?;

    let record = waive.record(
        decide_only_run(),
        NonZeroU64::new(7).ok_or("seven")?,
        ledger_time("2026-10-13T09:05:02.000Z"),
    );

    assert_eq!(record, Err(CommandTimeError::SubMillisecond));
    Ok(())
}

#[test]
fn a_record_whose_line_the_snapshot_cannot_judge_is_refused() -> Outcome {
    let record = approvers_waive()?.record(
        decide_only_run(),
        NonZeroU64::new(7).ok_or("seven")?,
        ledger_time("2026-10-13T09:05:02.000Z"),
    )?;

    for (text, problem) in [
        ("/assay recheck", UnreadCommand::Recheck),
        ("/assay approve", UnreadCommand::Unparsed),
        ("waive 0123456789abcdef", UnreadCommand::Unparsed),
        (
            "/assay waive 0123456789abcdef\nmore",
            UnreadCommand::NotCanonical,
        ),
    ] {
        let read = SnapshotCommand::try_from(&CommandRecord {
            command: text.to_owned(),
            ..record.clone()
        });

        assert_eq!(read, Err(problem), "{text:?}");
    }
    Ok(())
}
