//! What makes the GitHub state snapshot refuse (04 §5, wi-b5affa, H-13): ledger lists past
//! their caps, GitHub identities that do not answer the range, a range listed past 250 commits,
//! commands with no push clock or no author, and each failing port call named by its step.

use std::collections::BTreeSet;

use assay_app::{
    CommandRejected, CommitId, CommitIdentity, CommitRange, MAX_COMMAND_RECORDS, MAX_PUSH_RECORDS,
    MAX_RANGE_COMMITS, PushRecord, RangeCommit, RangeListing, ReviewState, Sha256Digest,
    SnapshotCommand, SnapshotStep, StateError,
};

use crate::eligibility_fakes::{
    APPROVER, By, CONTRIBUTOR, FIRST, FakeError, World, at, comment, created, fp, sha, user,
};

type Outcome = Result<(), Box<dyn std::error::Error>>;

/// `count` copies of the PR's opener push record.
fn opener_records(
    world: &World,
    count: usize,
) -> Result<Vec<PushRecord>, Box<dyn std::error::Error>> {
    let opener = world.pushes.first().ok_or("the PR has its opener record")?;
    Ok(vec![opener.clone(); count])
}

/// `count` command records of distinct comments.
fn command_records(count: u64) -> Result<Vec<SnapshotCommand>, Box<dyn std::error::Error>> {
    (1..=count)
        .map(|id| {
            Ok(SnapshotCommand {
                comment_id: comment(id, APPROVER, 5, "/assay waive 0123456789abcdef").id,
                actor: user(APPROVER),
                created_at: at(5),
                head_sha: sha(FIRST),
                body_sha256: Sha256Digest::of(b"/assay waive 0123456789abcdef"),
                command: assay_app::Command::Waive { finding: fp() },
            })
        })
        .collect()
}

/// Commit `n` of a long range: forty hex digits.
fn nth(n: usize) -> Result<CommitId, Box<dyn std::error::Error>> {
    Ok(format!("{n:040x}").parse()?)
}

#[test]
fn the_ledger_is_asked_for_at_most_each_cap() -> Outcome {
    let world = World::opened_by(CONTRIBUTOR);

    world.state()?;

    assert_eq!(
        *world.asked.borrow(),
        [
            (SnapshotStep::PushRecords, MAX_PUSH_RECORDS),
            (SnapshotStep::CommandRecords, MAX_COMMAND_RECORDS)
        ]
    );
    Ok(())
}

#[test]
fn push_records_at_the_cap_are_read() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.pushes = opener_records(&world, MAX_PUSH_RECORDS)?;

    let state = world.state();

    assert!(state.is_ok(), "{state:?}");
    Ok(())
}

#[test]
fn more_push_records_than_the_cap_refuse_the_snapshot() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.pushes = opener_records(&world, MAX_PUSH_RECORDS + 1)?;

    let state = world.state();

    assert!(
        matches!(state, Err(StateError::TooManyPushRecords)),
        "{state:?}"
    );
    Ok(())
}

#[test]
fn a_ledger_that_lists_past_the_cap_anyway_is_refused() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.pushes = opener_records(&world, MAX_PUSH_RECORDS + 1)?;
    world.commands = command_records(1)?;
    world.overfull_ledger = true;

    let pushes = world.state();
    world.pushes.truncate(1);
    world.commands = command_records(257)?;
    let commands = world.state();

    assert!(
        matches!(pushes, Err(StateError::TooManyPushRecords)),
        "{pushes:?}"
    );
    assert!(
        matches!(commands, Err(StateError::TooManyCommandRecords)),
        "{commands:?}"
    );
    Ok(())
}

#[test]
fn more_command_records_than_the_cap_refuse_the_snapshot() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commands = command_records(257)?;

    let state = world.state();

    assert_eq!(MAX_COMMAND_RECORDS, 256);
    assert!(
        matches!(state, Err(StateError::TooManyCommandRecords)),
        "{state:?}"
    );
    Ok(())
}

/// A two-commit PR whose identities the test replaces.
fn two_commits() -> World {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit('2', &[FIRST], By::user(CONTRIBUTOR));
    world.push((FIRST, '2'), CONTRIBUTOR, 5);
    world.head('2');
    world
}

/// The fake's identities of `commits`, in that order.
fn identities_of(world: &World, commits: &[char]) -> Result<Vec<CommitIdentity>, FakeError> {
    commits
        .iter()
        .map(|&id| {
            world
                .identities
                .get(&sha(id))
                .cloned()
                .ok_or(FakeError::NoSuchObject)
        })
        .collect()
}

#[test]
fn identities_short_of_the_range_refuse_the_snapshot() -> Outcome {
    let mut world = two_commits();
    world.identities_answer = Some(identities_of(&world, &['2'])?);

    let state = world.state();

    assert!(
        matches!(state, Err(StateError::IdentityMismatch)),
        "{state:?}"
    );
    Ok(())
}

#[test]
fn identities_out_of_order_refuse_the_snapshot() -> Outcome {
    let mut world = two_commits();
    world.identities_answer = Some(identities_of(&world, &[FIRST, '2'])?);

    let state = world.state();

    assert!(
        matches!(state, Err(StateError::IdentityMismatch)),
        "{state:?}"
    );
    Ok(())
}

#[test]
fn a_range_listed_past_250_commits_anyway_needs_a_split() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    let commits = (1..=MAX_RANGE_COMMITS + 1)
        .map(|n| {
            Ok(RangeCommit {
                commit: nth(n)?,
                parents: vec![nth(n - 1)?],
            })
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    world.range = RangeListing::Listed(commits);
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));

    let state = world.state()?;

    assert_eq!(state.commits(), CommitRange::NeedsSplit);
    assert_eq!(
        state.eligibility(user(APPROVER)),
        Err(assay_app::Ineligible::RangeTooLarge)
    );
    Ok(())
}

#[test]
fn a_command_on_a_head_with_no_push_record_is_rejected() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.pushes.clear();
    let state = world.state()?;

    let recorded = state.record_command(&created(comment(
        40,
        APPROVER,
        5,
        "/assay waive 0123456789abcdef",
    )));

    assert_eq!(recorded, Err(CommandRejected::NoPushClock));
    Ok(())
}

#[test]
fn a_command_comment_with_no_author_is_rejected() -> Outcome {
    let state = World::opened_by(CONTRIBUTOR).state()?;
    let mut posted = comment(40, APPROVER, 5, "/assay waive 0123456789abcdef");
    posted.user = None;

    let recorded = state.record_command(&created(posted));

    assert_eq!(recorded, Err(CommandRejected::NoAuthor));
    Ok(())
}

/// A PR whose head is a merge candidate, so every port call is reached.
fn every_call() -> World {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit('c', &[FIRST, 'd'], By::user(CONTRIBUTOR));
    world.push((FIRST, 'c'), CONTRIBUTOR, 5);
    world.head('c');
    world
}

#[test]
fn a_failing_port_call_names_its_step() -> Outcome {
    let steps = [
        SnapshotStep::PushRecords,
        SnapshotStep::RangeList,
        SnapshotStep::MergeCheck,
        SnapshotStep::Identities,
        SnapshotStep::Reviews,
        SnapshotStep::CommandRecords,
    ];
    let mut named = BTreeSet::new();
    for step in steps {
        let mut world = every_call();
        world.fail = Some(step);

        let state = world.state();

        let failed = match state {
            Err(
                StateError::Forge { step, .. }
                | StateError::Range { step, .. }
                | StateError::Ledger { step, .. },
            ) => step,
            other => return Err(format!("{step:?}: {other:?}").into()),
        };
        assert_eq!(failed, step);
        named.insert(format!("{failed:?}"));
    }
    assert_eq!(named.len(), steps.len());
    Ok(())
}
