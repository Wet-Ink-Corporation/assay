//! `LedgerPublish`, `publish`'s ledger port over the ledger and lease operations (D15): the
//! per-head lock through `acquire_lock` with bounded retries and released by compare-and-set,
//! and the evidence pointer and record through `update_state` (02 §5, R3-08, R3-09, R3-17).

use std::assert_matches;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    EvidenceEntry, EvidencePointer, EvidenceRecord, LedgerPublish, LedgerPublishError, LlmEvidence,
    LockHold, LockLease, MAX_LOCK_RETRIES, MAX_STATE_ENTRIES, PrState, PublishLedger, StateUpdate,
    read_state, update_state,
};
use assay_domain::Tier;

use crate::lease_fakes::{Before, Call, MemoryRefs};
use crate::ledger_fakes::{MemoryLedger, at, commit, digest, id, run};
use crate::open_fakes::FakeClock;

/// The instant every attempt reads.
const NOW: &str = "2026-10-12T09:30:00.000Z";
/// The repository the run publishes for.
const REPO: &str = "acme/widgets";

/// The lock of head `bbbb…` in repository 42.
fn lock_ref() -> String {
    format!("refs/heads/lock-42-{}", "b".repeat(40))
}

/// The lock line of run `run_id` on head `bbbb…`, expiring at `expires_at`.
fn lock_line(run_id: u64, expires_at: &str) -> String {
    format!(
        r#"{{"schema":"assay.ledger-lock/1","run_uid":{{"repo_id":42,"run_id":{run_id},"run_attempt":1}},"repo":"acme/widgets","head_sha":"{}","expires_at":"{expires_at}"}}"#,
        "b".repeat(40)
    )
}

/// Run 7's publish ledger over `ledger`, `refs` and `clock`.
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
        repo: REPO,
    }
}

/// How many lock creations were attempted.
fn creations(refs: &MemoryRefs) -> usize {
    refs.calls()
        .iter()
        .filter(|call| matches!(call, Call::Create { .. }))
        .count()
}

#[test]
fn acquire_takes_the_lock_with_this_runs_two_minute_lease() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );

    let lease = publisher(&ledger, &refs, &clock)
        .acquire(id(42), &commit('b'))
        .expect("the ledger is up")
        .expect("a free lock is taken");

    let (lock_commit, message) = refs.get(&lock_ref()).expect("the lock ref was created");
    assert_eq!(lease.lock_commit, lock_commit);
    assert_eq!(message, lock_line(7, "2026-10-12T09:32:00.000Z"));
    assert_eq!(clock.pauses(), Vec::<u8>::new());
}

#[test]
fn a_contended_lock_gives_none_after_its_bounded_retries() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    let other = lock_line(9, "2026-10-12T09:31:00.000Z");
    refs.holding(&lock_ref(), &other);

    let lease = publisher(&ledger, &refs, &clock)
        .acquire(id(42), &commit('b'))
        .expect("the ledger is up");

    assert_eq!(lease, None);
    assert_eq!(clock.pauses(), (1..=MAX_LOCK_RETRIES).collect::<Vec<_>>());
    assert_eq!(creations(&refs), usize::from(MAX_LOCK_RETRIES) + 1);
    assert_eq!(refs.get(&lock_ref()).map(|(_, line)| line), Some(other));
}

#[test]
fn a_live_lock_of_this_run_is_busy_too_so_two_jobs_never_both_hold_it() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    refs.holding(&lock_ref(), &lock_line(7, "2026-10-12T09:31:00.000Z"));

    let lease = publisher(&ledger, &refs, &clock)
        .acquire(id(42), &commit('b'))
        .expect("the ledger is up");

    assert_eq!(lease, None);
}

#[test]
fn a_lock_freed_between_attempts_is_taken_on_the_next() {
    let ledger = MemoryLedger::default();
    let clock = FakeClock::reading(&[NOW]);
    let name = lock_ref();
    let refs = MemoryRefs::default()
        .rival(Before::Create, |_| {})
        .rival(Before::Create, move |store| store.remove(&name));
    refs.holding(&lock_ref(), &lock_line(9, "2026-10-12T09:31:00.000Z"));

    let lease = publisher(&ledger, &refs, &clock)
        .acquire(id(42), &commit('b'))
        .expect("the ledger is up");

    assert_matches!(lease, Some(_));
    assert_eq!(clock.pauses(), vec![1]);
}

#[test]
fn acquire_for_another_repository_is_refused_before_any_ref_call() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );

    let lease = publisher(&ledger, &refs, &clock).acquire(id(43), &commit('b'));

    assert_matches!(
        lease,
        Err(LedgerPublishError::OtherRepository { repo_id }) if repo_id == id(43)
    );
    assert_eq!(refs.calls(), Vec::new());
}

#[test]
fn release_deletes_the_lock_it_holds() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    let publish = publisher(&ledger, &refs, &clock);
    let lease = publish
        .acquire(id(42), &commit('b'))
        .expect("the ledger is up")
        .expect("a free lock is taken");

    publish.release(lease).expect("the lock is released");

    assert_eq!(refs.names(), Vec::<String>::new());
}

#[test]
fn a_lock_another_writer_retook_is_not_held_and_keeps_its_lease() {
    let ledger = MemoryLedger::default();
    let clock = FakeClock::reading(&[NOW]);
    let name = lock_ref();
    let theirs = lock_line(9, "2026-10-12T09:34:00.000Z");
    let retaken = theirs.clone();
    let refs = MemoryRefs::default().rival(Before::Delete, move |store| {
        store.remove(&name);
        store.create(&name, &retaken);
    });
    let publish = publisher(&ledger, &refs, &clock);
    let lease = publish
        .acquire(id(42), &commit('b'))
        .expect("the ledger is up")
        .expect("a free lock is taken");

    let released = publish.release(lease);

    assert_matches!(released, Err(LedgerPublishError::NotHeld));
    assert_eq!(refs.get(&lock_ref()).map(|(_, line)| line), Some(theirs));
}

/// Run `run_id`'s evidence entry for PR 7 of repository 42: evidence key `key`, bundle `bundle`.
fn entry(run_id: u64, key: char, bundle: char) -> EvidenceEntry {
    EvidenceEntry {
        repo_id: id(42),
        pr: id(7),
        pointer: EvidencePointer {
            run_id: id(run_id),
            run_attempt: NonZeroU64::MIN,
            sha256: digest(bundle),
        },
        record: record(key, bundle, LlmEvidence::Complete),
    }
}

/// An evidence record of key `key` for bundle `bundle`, at tier `lite`.
fn record(key: char, bundle: char, llm: LlmEvidence) -> EvidenceRecord {
    EvidenceRecord {
        evidence_key: digest(key).to_string(),
        bundle_sha256: digest(bundle),
        llm,
        tier: Tier::Lite,
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

/// PR 7's state in `ledger`.
fn state(ledger: &MemoryLedger) -> Result<PrState, Box<dyn Error>> {
    Ok(read_state(ledger, id(42), id(7))?)
}

/// Run 7's hold of the lock of head `bbbb…`, its lease expiring at `expires_at`.
fn hold(expires_at: &str) -> LockHold {
    LockHold {
        lock_commit: commit('c'),
        lease: LockLease {
            run_uid: run(7),
            repo: REPO.to_owned(),
            head_sha: commit('b'),
            expires_at: at(expires_at),
        },
    }
}

/// A hold whose lease is live at [`NOW`].
fn live() -> LockHold {
    hold("2026-10-12T09:32:00.000Z")
}

// #332 (#331 F3): the evidence is written only while the lease is live, as `open`'s writes are.
#[test]
fn evidence_at_or_past_the_lease_s_deadline_is_refused_unwritten() {
    for expires_at in [NOW, "2026-10-12T09:29:59.999Z"] {
        let (ledger, refs, clock) = (
            MemoryLedger::default(),
            MemoryRefs::default(),
            FakeClock::reading(&[NOW]),
        );
        let expired = hold(expires_at);

        let recorded =
            publisher(&ledger, &refs, &clock).record_evidence(&expired, &entry(7, 'e', 'a'));

        assert_matches!(
            recorded,
            Err(LedgerPublishError::PastDeadline { deadline }) if deadline == expired.deadline(),
            "{expires_at}"
        );
        assert_eq!(ledger.writes(), Vec::new(), "{expires_at}");
    }
}

#[test]
fn an_unreadable_clock_records_no_evidence() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::broken(),
    );

    let recorded = publisher(&ledger, &refs, &clock).record_evidence(&live(), &entry(7, 'e', 'a'));

    assert_matches!(recorded, Err(LedgerPublishError::Clock(_)));
    assert_eq!(ledger.writes(), Vec::new());
}

#[test]
fn the_deadline_of_a_lease_is_its_hold_s() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    let held = live();

    assert_eq!(
        publisher(&ledger, &refs, &clock).deadline(&held),
        held.deadline()
    );
}

#[test]
fn recording_evidence_writes_the_pointer_and_the_record() -> Result<(), Box<dyn Error>> {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    let recorded = entry(7, 'e', 'a');

    publisher(&ledger, &refs, &clock).record_evidence(&live(), &recorded)?;

    assert_eq!(
        state(&ledger)?,
        PrState {
            evidence: Some(recorded.pointer),
            evidence_records: vec![recorded.record],
            ..PrState::default()
        }
    );
    Ok(())
}

#[test]
fn a_record_of_the_same_evidence_key_is_replaced_in_place() -> Result<(), Box<dyn Error>> {
    let kept = record('c', 'c', LlmEvidence::Complete);
    let ledger = ledger_with(&PrState {
        evidence_records: vec![record('e', 'd', LlmEvidence::Deferred), kept.clone()],
        ..PrState::default()
    })?;
    let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW]));
    let recorded = entry(7, 'e', 'a');

    publisher(&ledger, &refs, &clock).record_evidence(&live(), &recorded)?;

    assert_eq!(
        state(&ledger)?.evidence_records,
        vec![recorded.record, kept]
    );
    Ok(())
}

#[test]
fn a_full_record_list_drops_its_oldest_record_for_the_new_one() -> Result<(), Box<dyn Error>> {
    let full: Vec<EvidenceRecord> = (0..MAX_STATE_ENTRIES)
        .map(|n| EvidenceRecord {
            evidence_key: format!("sha256:{n:064x}"),
            ..record('0', 'd', LlmEvidence::Complete)
        })
        .collect();
    let ledger = ledger_with(&PrState {
        evidence_records: full.clone(),
        ..PrState::default()
    })?;
    let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW]));
    let recorded = entry(7, 'e', 'a');

    publisher(&ledger, &refs, &clock).record_evidence(&live(), &recorded)?;

    let mut expected = full.get(1..).expect("256 records").to_vec();
    expected.push(recorded.record);
    assert_eq!(state(&ledger)?.evidence_records, expected);
    Ok(())
}

#[test]
fn an_older_run_records_its_evidence_but_never_moves_the_pointer_back() -> Result<(), Box<dyn Error>>
{
    let newer = entry(9, 'f', 'f');
    let ledger = ledger_with(&PrState {
        evidence: Some(newer.pointer.clone()),
        evidence_records: vec![newer.record.clone()],
        ..PrState::default()
    })?;
    let (refs, clock) = (MemoryRefs::default(), FakeClock::reading(&[NOW]));
    let recorded = entry(7, 'e', 'a');

    publisher(&ledger, &refs, &clock).record_evidence(&live(), &recorded)?;

    assert_eq!(
        state(&ledger)?,
        PrState {
            evidence: Some(newer.pointer),
            evidence_records: vec![newer.record, recorded.record],
            ..PrState::default()
        }
    );
    Ok(())
}

#[test]
fn an_entry_of_another_run_or_repository_is_refused_unwritten() {
    let (ledger, refs, clock) = (
        MemoryLedger::default(),
        MemoryRefs::default(),
        FakeClock::reading(&[NOW]),
    );
    let publish = publisher(&ledger, &refs, &clock);
    let other_run = entry(8, 'e', 'a');
    let other_repository = EvidenceEntry {
        repo_id: id(43),
        ..entry(7, 'e', 'a')
    };
    let retry = EvidenceEntry {
        pointer: EvidencePointer {
            run_attempt: id(2),
            ..entry(7, 'e', 'a').pointer
        },
        ..entry(7, 'e', 'a')
    };

    assert_matches!(
        publish.record_evidence(&live(), &other_run),
        Err(LedgerPublishError::OtherRun)
    );
    assert_matches!(
        publish.record_evidence(&live(), &retry),
        Err(LedgerPublishError::OtherRun)
    );
    assert_matches!(
        publish.record_evidence(&live(), &other_repository),
        Err(LedgerPublishError::OtherRepository { repo_id }) if repo_id == id(43)
    );
    assert_eq!(ledger.writes(), Vec::new());
}
