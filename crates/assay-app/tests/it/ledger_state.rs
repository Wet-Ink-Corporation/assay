//! Per-PR state, `state/<repo_id>/<pr>.json`: pointers, evidence records, the deferral record and
//! per-actor counters, changed only by compare-and-set on the file (06 §1, ADR-23, R3-08, R3-11,
//! R3-15).

use std::error::Error;

use assay_app::{
    ActorCounter, Corruption, DecisionPointer, DeferralRecord, EvidencePointer, EvidenceRecord,
    LedgerPath, LedgerReadError, LedgerWriteError, LlmEvidence, MAX_STATE_ENTRIES, PrState,
    StateUpdate, UserId, WriteCondition, WriteOutcome, read_state, update_state,
};
use assay_domain::Tier;

use crate::ledger_fakes::{MemoryLedger, at, commit, digest, id};

fn path() -> LedgerPath {
    LedgerPath::state(id(42), id(7))
}

fn evidence_record(digit: char) -> EvidenceRecord {
    EvidenceRecord {
        evidence_key: digest(digit).to_string(),
        bundle_sha256: digest('e'),
        llm: LlmEvidence::Complete,
        tier: Tier::Lite,
    }
}

fn counted(actor: u64, count: u32) -> ActorCounter {
    ActorCounter {
        actor_id: UserId(id(actor)),
        count,
    }
}

/// `state` with one more command counted for `actor`.
fn count_command(state: &PrState, actor: u64) -> PrState {
    let mut next = state.clone();
    match next
        .actor_counters
        .iter_mut()
        .find(|counter| counter.actor_id == UserId(id(actor)))
    {
        Some(counter) => counter.count += 1,
        None => next.actor_counters.push(counted(actor, 1)),
    }
    next
}

fn full_state() -> PrState {
    PrState {
        evidence: Some(EvidencePointer {
            run_id: id(100),
            run_attempt: id(1),
            sha256: digest('e'),
        }),
        decision: Some(DecisionPointer {
            run_id: id(101),
            sha256: digest('d'),
        }),
        evidence_records: vec![evidence_record('a')],
        deferral: Some(DeferralRecord {
            repo: "acme/widgets".to_owned(),
            pr: id(7),
            head: commit('b'),
            count: 2,
            resets_at: Some(at("2026-10-12T14:00:00.000Z")),
            flag: true,
        }),
        actor_counters: vec![counted(1001, 3)],
    }
}

fn full_document() -> String {
    format!(
        concat!(
            r#"{{"schema":"assay.ledger-state/1","repo_id":42,"pr":7,"#,
            r#""evidence":{{"run_id":100,"run_attempt":1,"sha256":"sha256:{e}"}},"#,
            r#""decision":{{"run_id":101,"sha256":"sha256:{d}"}},"#,
            r#""evidence_records":[{{"evidence_key":"sha256:{a}","bundle_sha256":"sha256:{e}","#,
            r#""llm":"complete","tier":"lite"}}],"#,
            r#""deferral":{{"repo":"acme/widgets","pr":7,"head":"{b}","count":2,"#,
            r#""resets_at":"2026-10-12T14:00:00.000Z","flag":true}},"#,
            r#""actor_counters":[{{"actor_id":1001,"count":3}}]}}"#,
            "\n"
        ),
        a = "a".repeat(64),
        b = "b".repeat(40),
        d = "d".repeat(64),
        e = "e".repeat(64),
    )
}

const EMPTY_DOCUMENT: &str = concat!(
    r#"{"schema":"assay.ledger-state/1","repo_id":42,"pr":7,"evidence_records":[],"#,
    r#""actor_counters":[]}"#,
    "\n"
);

#[test]
fn a_pr_without_a_state_file_has_the_empty_state() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    assert_eq!(read_state(&ledger, id(42), id(7))?, PrState::default());
    Ok(())
}

#[test]
fn the_first_update_creates_the_state_file() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    let updated = update_state(&ledger, id(42), id(7), |_state| Some(full_state()))?;

    assert_eq!(updated, StateUpdate::Updated(full_state()));
    assert_eq!(path().as_str(), "state/42/7.json");
    assert_eq!(ledger.text(&path()), full_document());
    assert_eq!(read_state(&ledger, id(42), id(7))?, full_state());
    assert_eq!(
        ledger
            .writes()
            .into_iter()
            .map(|write| write.condition)
            .collect::<Vec<_>>(),
        [WriteCondition::Absent]
    );
    Ok(())
}

#[test]
fn the_empty_state_is_written_without_its_absent_parts() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default().with_file(&path(), full_document().as_bytes());

    update_state(&ledger, id(42), id(7), |_state| Some(PrState::default()))?;

    assert_eq!(ledger.text(&path()), EMPTY_DOCUMENT);
    Ok(())
}

#[test]
fn a_concurrent_update_is_applied_again_to_the_fresh_state() -> Result<(), Box<dyn Error>> {
    let rival = EMPTY_DOCUMENT.replace(
        r#""actor_counters":[]"#,
        r#""actor_counters":[{"actor_id":1001,"count":1}]"#,
    );
    let ledger = MemoryLedger::default().rival_writes(rival.as_bytes());

    let updated = update_state(&ledger, id(42), id(7), |state| {
        Some(count_command(state, 1001))
    })?;

    let expected = PrState {
        actor_counters: vec![counted(1001, 2)],
        ..PrState::default()
    };
    assert_eq!(updated, StateUpdate::Updated(expected.clone()));
    assert_eq!(read_state(&ledger, id(42), id(7))?, expected);
    let outcomes: Vec<_> = ledger.writes().into_iter().map(|w| w.outcome).collect();
    assert_eq!(outcomes, [WriteOutcome::Conflict, WriteOutcome::Written]);
    Ok(())
}

#[test]
fn an_update_that_changes_nothing_writes_nothing() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default().with_file(&path(), full_document().as_bytes());

    let kept = update_state(&ledger, id(42), id(7), |_state| None)?;
    let same = update_state(&ledger, id(42), id(7), |state| Some(state.clone()))?;

    assert_eq!(kept, StateUpdate::Unchanged(full_state()));
    assert_eq!(same, StateUpdate::Unchanged(full_state()));
    assert_eq!(ledger.writes(), []);
    Ok(())
}

/// Reading a state file holding `contents` is refused for `problem`.
fn state_refused(contents: &str, problem: Corruption) {
    let ledger = MemoryLedger::default().with_file(&path(), contents.as_bytes());

    let read = read_state(&ledger, id(42), id(7));

    assert!(
        matches!(
            &read,
            Err(LedgerReadError::Corrupt { line: 1, problem: found, .. }) if *found == problem
        ),
        "{contents:?}: {read:?}"
    );
}

#[test]
fn a_state_document_written_as_an_array_is_refused() {
    state_refused(
        r#"["assay.ledger-state/1",42,7,null,null,[],null,[]]"#,
        Corruption::Shape,
    );
}

#[test]
fn a_state_file_of_another_pr_is_refused() {
    state_refused(
        &full_document().replacen(r#""pr":7,"evidence""#, r#""pr":8,"evidence""#, 1),
        Corruption::WrongPullRequest,
    );
}

#[test]
fn a_deferral_record_of_another_pr_is_refused() {
    state_refused(
        &full_document().replacen(r#""pr":7,"head""#, r#""pr":8,"head""#, 1),
        Corruption::WrongPullRequest,
    );
}

#[test]
fn a_state_file_that_repeats_a_key_is_refused() {
    state_refused(
        &full_document().replacen(r#""pr":7,"#, r#""pr":7,"pr":7,"#, 1),
        Corruption::Shape,
    );
}

#[test]
fn a_state_file_with_an_unknown_llm_state_is_refused() {
    state_refused(
        &full_document().replacen("complete", "completed", 1),
        Corruption::Field("llm"),
    );
}

#[test]
fn a_state_file_that_counts_an_actor_twice_is_refused() {
    state_refused(
        &full_document().replacen(
            r#"{"actor_id":1001,"count":3}"#,
            r#"{"actor_id":1001,"count":3},{"actor_id":1001,"count":1}"#,
            1,
        ),
        Corruption::Duplicate,
    );
}

#[test]
fn a_state_file_with_an_evidence_key_twice_is_refused() {
    let record = format!(
        r#"{{"evidence_key":"sha256:{a}","bundle_sha256":"sha256:{e}","llm":"complete","tier":"lite"}}"#,
        a = "a".repeat(64),
        e = "e".repeat(64),
    );
    state_refused(
        &full_document().replacen(&record, &format!("{record},{record}"), 1),
        Corruption::Duplicate,
    );
}

#[test]
fn a_state_past_its_entry_cap_is_never_written() {
    let ledger = MemoryLedger::default();
    let many = PrState {
        actor_counters: (1..=u64::try_from(MAX_STATE_ENTRIES).expect("a small cap") + 1)
            .map(|actor| counted(actor, 1))
            .collect(),
        ..PrState::default()
    };

    let updated = update_state(&ledger, id(42), id(7), |_state| Some(many.clone()));

    assert!(
        matches!(
            &updated,
            Err(LedgerWriteError::Invalid {
                problem: Corruption::Field("actor_counters"),
                ..
            })
        ),
        "{updated:?}"
    );
    assert_eq!(ledger.writes(), []);
}
