//! The publication register through `LedgerPublish` (#61 DO-7, D2 A): every verdict writer
//! records `{head_sha, key, run_id}` under the per-head lock, and decide-only records its
//! decision pointer with it, only while the register and evidence pointer it claimed stand.

use std::assert_matches;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    Claim, Claimed, DecisionEntry, DecisionPointer, EvidencePointer, LedgerPublish,
    LedgerPublishError, LockHold, LockLease, PrState, PublicationRegister, PublishLedger,
    PublishedEntry, StateUpdate, read_state, update_state,
};

use crate::lease_fakes::MemoryRefs;
use crate::ledger_fakes::{MemoryLedger, at, commit, digest, id, run};
use crate::open_fakes::FakeClock;

/// The instant every clock read gives.
const NOW: &str = "2026-10-12T09:30:00.000Z";

/// Run 7's publish ledger over `ledger`.
fn publisher<'a>(
    ledger: &'a MemoryLedger,
    refs: &'a MemoryRefs,
    clock: &'a FakeClock,
) -> LedgerPublish<'a, MemoryLedger, MemoryRefs, FakeClock> {
    LedgerPublish {
        ledger,
        refs,
        clock,
        run_uid: run(7),
        repo: "acme/widgets",
    }
}

/// Run 7's hold of the lock of head `bbbb…`, expiring at `expires_at`.
fn hold(expires_at: &str) -> LockHold {
    LockHold {
        lock_commit: commit('c'),
        lease: LockLease {
            run_uid: run(7),
            repo: "acme/widgets".to_owned(),
            head_sha: commit('b'),
            expires_at: at(expires_at),
        },
    }
}

/// A hold live at [`NOW`].
fn live() -> LockHold {
    hold("2026-10-12T09:32:00.000Z")
}

/// The register of head `head`, key `key`, written by run `run_id`.
fn register(head: char, key: char, run_id: u64) -> PublicationRegister {
    PublicationRegister {
        head_sha: commit(head),
        key: digest(key),
        run_id: id(run_id),
    }
}

/// The evidence pointer of pipeline run `run_id`.
fn evidence(run_id: u64) -> EvidencePointer {
    EvidencePointer {
        run_id: id(run_id),
        run_attempt: NonZeroU64::MIN,
        sha256: digest('e'),
    }
}

/// What decide-only read: pipeline run 5's register of head `bbbb…` and its evidence.
fn claim() -> Claim {
    Claim {
        register: register('b', 'a', 5),
        evidence: evidence(5),
    }
}

/// The state decide-only read [`claim`] from.
fn claimed_state() -> PrState {
    PrState {
        evidence: Some(evidence(5)),
        published: Some(register('b', 'a', 5)),
        ..PrState::default()
    }
}

/// Run 7's register entry for PR 7 of repository 42, over `over`.
fn published(register: PublicationRegister, over: Option<Claim>) -> PublishedEntry {
    PublishedEntry {
        repo_id: id(42),
        pr: id(7),
        register,
        over,
    }
}

/// Run 7's decision entry: its bundle `dddd…`, keyed `ffff…`, over [`claim`].
fn decision() -> DecisionEntry {
    DecisionEntry {
        repo_id: id(42),
        pr: id(7),
        pointer: DecisionPointer {
            run_id: id(7),
            sha256: digest('d'),
        },
        register: register('b', 'f', 7),
        over: claim(),
    }
}

/// A ledger whose state for PR 7 of repository 42 is `state`.
fn ledger_with(state: &PrState) -> Result<MemoryLedger, Box<dyn Error>> {
    let ledger = MemoryLedger::default();
    // The update hands the ledger a state of its own.
    match update_state(&ledger, id(42), id(7), |_| Some(state.clone()))? {
        StateUpdate::Updated(_) => Ok(ledger),
        StateUpdate::Unchanged(_) => Err("the starting state is not empty".into()),
    }
}

fn state(ledger: &MemoryLedger) -> Result<PrState, Box<dyn Error>> {
    Ok(read_state(ledger, id(42), id(7))?)
}

#[test]
fn a_pipeline_register_is_written_alone_over_whatever_was_there() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(&claimed_state())?;
    let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW]));

    let recorded = publisher(&ledger, &refs, &clock)
        .record_published(&live(), &published(register('b', 'c', 7), None))?;

    assert_eq!(recorded, Claimed::Recorded);
    assert_eq!(
        state(&ledger)?,
        PrState {
            published: Some(register('b', 'c', 7)),
            ..claimed_state()
        }
    );
    Ok(())
}

#[test]
fn a_late_pipeline_register_leaves_a_later_run_s_register_of_another_head()
-> Result<(), Box<dyn Error>> {
    // (register there, whether run 7's pipeline register of head `bbbb…` replaces it)
    let cases = [
        (register('a', 'f', 9), false),
        (register('a', 'f', 5), true),
        (register('b', 'f', 9), true),
    ];
    for (there, replaced) in cases {
        let before = PrState {
            // The state owns its copy; `there` is compared after the write.
            published: Some(there.clone()),
            ..claimed_state()
        };
        let ledger = ledger_with(&before)?;
        let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW]));

        let recorded = publisher(&ledger, &refs, &clock)
            .record_published(&live(), &published(register('b', 'c', 7), None))?;

        assert_eq!(recorded, Claimed::Recorded, "{there:?}");
        let kept = if replaced {
            register('b', 'c', 7)
        } else {
            there
        };
        assert_eq!(state(&ledger)?.published, Some(kept));
    }
    Ok(())
}

#[test]
fn a_register_of_another_head_than_the_lock_s_is_refused_unwritten() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    let publish = publisher(&ledger, &refs, &clock);
    let other_head = DecisionEntry {
        register: register('c', 'f', 7),
        ..decision()
    };

    assert_matches!(
        publish.record_published(&live(), &published(register('c', 'c', 7), None)),
        Err(LedgerPublishError::OtherHead)
    );
    assert_matches!(
        publish.record_decision(&live(), &other_head),
        Err(LedgerPublishError::OtherHead)
    );
    assert_eq!(ledger.writes(), Vec::new());
}

#[test]
fn a_register_or_pointer_of_another_run_or_repository_is_refused_unwritten() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    let publish = publisher(&ledger, &refs, &clock);
    let other_pointer = DecisionEntry {
        pointer: DecisionPointer {
            run_id: id(8),
            sha256: digest('d'),
        },
        ..decision()
    };
    let other_repository = PublishedEntry {
        repo_id: id(43),
        ..published(register('b', 'c', 7), None)
    };

    assert_matches!(
        publish.record_published(&live(), &published(register('b', 'c', 8), None)),
        Err(LedgerPublishError::OtherRun)
    );
    assert_matches!(
        publish.record_decision(&live(), &other_pointer),
        Err(LedgerPublishError::OtherRun)
    );
    assert_matches!(
        publish.record_published(&live(), &other_repository),
        Err(LedgerPublishError::OtherRepository { repo_id }) if repo_id == id(43)
    );
    assert_eq!(ledger.writes(), Vec::new());
}

#[test]
fn a_claim_whose_register_or_evidence_moved_writes_nothing() -> Result<(), Box<dyn Error>> {
    let moved = [
        PrState {
            published: Some(register('b', 'c', 6)),
            ..claimed_state()
        },
        PrState {
            evidence: Some(evidence(6)),
            ..claimed_state()
        },
        PrState::default(),
    ];
    for current in moved {
        let ledger = MemoryLedger::default();
        if current != PrState::default() {
            update_state(&ledger, id(42), id(7), |_| Some(current.clone()))?;
        }
        let before = ledger.writes().len();
        let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW, NOW]));
        let publish = publisher(&ledger, &refs, &clock);

        let refusal =
            publish.record_published(&live(), &published(register('b', 'c', 7), Some(claim())))?;
        let decided = publish.record_decision(&live(), &decision())?;

        assert_eq!(
            (refusal, decided),
            (Claimed::Moved, Claimed::Moved),
            "{current:?}"
        );
        assert_eq!(ledger.writes().len(), before, "{current:?}");
        assert_eq!(state(&ledger)?, current);
    }
    Ok(())
}

#[test]
fn a_decision_writes_its_pointer_and_register_in_one_write() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(&claimed_state())?;
    let before = ledger.writes().len();
    let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW]));

    let recorded = publisher(&ledger, &refs, &clock).record_decision(&live(), &decision())?;

    assert_eq!(recorded, Claimed::Recorded);
    assert_eq!(ledger.writes().len(), before.saturating_add(1));
    assert_eq!(
        state(&ledger)?,
        PrState {
            decision: Some(decision().pointer),
            published: Some(register('b', 'f', 7)),
            ..claimed_state()
        }
    );
    Ok(())
}

#[test]
fn a_decide_only_refusal_writes_its_register_over_the_claim() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(&claimed_state())?;
    let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW]));

    let recorded = publisher(&ledger, &refs, &clock)
        .record_published(&live(), &published(register('b', 'c', 7), Some(claim())))?;

    assert_eq!(recorded, Claimed::Recorded);
    assert_eq!(state(&ledger)?.published, Some(register('b', 'c', 7)));
    assert_eq!(state(&ledger)?.decision, None, "a refusal moves no pointer");
    Ok(())
}

#[test]
fn a_register_at_or_past_the_lease_s_deadline_is_refused_unwritten() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW, NOW]),
    );
    let publish = publisher(&ledger, &refs, &clock);
    let expired = hold(NOW);

    assert_matches!(
        publish.record_published(&expired, &published(register('b', 'c', 7), None)),
        Err(LedgerPublishError::PastDeadline { deadline }) if deadline == expired.deadline()
    );
    assert_matches!(
        publish.record_decision(&expired, &decision()),
        Err(LedgerPublishError::PastDeadline { deadline }) if deadline == expired.deadline()
    );
    assert_eq!(ledger.writes(), Vec::new());
}
