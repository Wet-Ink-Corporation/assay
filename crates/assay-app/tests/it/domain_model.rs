//! The domain's contract types write documents their schemas accept: a sample of every event
//! kind, and every valid golden fixture after a trip through the domain type (03 §3, §6).

use std::collections::BTreeSet;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{Contract, ContractValidator};
use assay_domain::{
    Disposition, Event, EventKind, EventPayload, EventSchema, FeedbackSignal, FindingsDocument,
    GateConclusion, GateRun, Preflight, RateLimitStatus, RunUid, Severity, SpecialistRun,
    VerifierVerdict, VoidReason,
};
use serde_json::Value;

use crate::support::{Validity, fixtures};

fn id(n: u64) -> Result<NonZeroU64, Box<dyn Error>> {
    Ok(NonZeroU64::new(n).ok_or("ids are positive")?)
}

fn run_uid(run_id: u64) -> Result<RunUid, Box<dyn Error>> {
    Ok(RunUid {
        repo_id: id(840_213_377)?,
        run_id: id(run_id)?,
        run_attempt: id(1)?,
    })
}

fn gate_run() -> GateRun {
    GateRun {
        plugin: "temper".to_owned(),
        exit: Some(1),
        conclusion: Some(GateConclusion::Fail),
        duration_ms: Some(81_234),
    }
}

fn preflight() -> Preflight {
    Preflight {
        auth_method: "oauth".to_owned(),
        cli_version: "2.1.296".to_owned(),
        init_clean: true,
    }
}

fn specialist_run() -> SpecialistRun {
    SpecialistRun {
        role: "bugs".to_owned(),
        model: Some("claude".to_owned()),
        input_tokens: Some(12_000),
        output_tokens: Some(800),
        cache_read_tokens: Some(9_000),
        cost_aeu: Some(0.25),
        duration_ms: Some(40_000),
        turns: Some(6),
        stop_reason: Some("end_turn".to_owned()),
    }
}

/// Samples of the kinds that carry no data, or the data of a run's own progress.
fn run_samples() -> Vec<EventPayload> {
    vec![
        EventPayload::RunRequested {},
        EventPayload::PlanComputed {},
        EventPayload::Admitted {
            slot: "slot-1".to_owned(),
            reserved_aeu: 1.5,
        },
        EventPayload::PreflightPassed(preflight()),
        EventPayload::PreflightFailed(preflight()),
        EventPayload::GateStarted(gate_run()),
        EventPayload::GateFinished(gate_run()),
        EventPayload::SpecialistStarted(specialist_run()),
        EventPayload::SpecialistFinished(specialist_run()),
        EventPayload::SpecialistFailed(specialist_run()),
        EventPayload::SpecialistBudgetExhausted(specialist_run()),
        EventPayload::CoordinatorFinished {},
        EventPayload::StageDegraded {
            stage: "review".to_owned(),
            reason: "infra-llm-unavailable".to_owned(),
        },
        EventPayload::RateLimitObserved {
            status: RateLimitStatus::Rejected,
            resets_at: Some("2026-10-11T07:00:00Z".to_owned()),
        },
        EventPayload::DecisionMade {},
        EventPayload::CheckPublished {},
        EventPayload::BundlePersisted {
            sha256: "c".repeat(64),
        },
        EventPayload::RunSuperseded {},
        EventPayload::BudgetDeferred {},
        EventPayload::SlotBusy {},
        EventPayload::LedgerWritten {},
        EventPayload::SlotReleased {},
        EventPayload::EvidenceReused {
            evidence_key: "run-41-1".to_owned(),
        },
    ]
}

/// Samples of the kinds about findings, owners' actions, pushes and commands.
fn finding_and_command_samples() -> Result<Vec<EventPayload>, Box<dyn Error>> {
    Ok(vec![
        EventPayload::FindingRaised {},
        EventPayload::FindingVerified {
            verdict: VerifierVerdict::Refuted,
        },
        EventPayload::FindingDisposed {
            code: Disposition::VerifierRejectedDowngraded,
            from: Some(Severity::Major),
            to: Some(Severity::Minor),
        },
        EventPayload::FindingPublished {
            url: "https://github.com/o/r/pull/42#discussion_r1".to_owned(),
        },
        EventPayload::WaiverApplied {
            who: "owner".to_owned(),
            fp: "0123456789abcdef".to_owned(),
        },
        EventPayload::OverrideApplied {
            who: "owner".to_owned(),
            reason: "llm".to_owned(),
        },
        EventPayload::FeedbackReceived {
            signal: FeedbackSignal::SelfDismiss,
        },
        EventPayload::SlotReaped {
            run_uid: run_uid(18_341_000_002)?,
        },
        EventPayload::PushRecorded {
            before: "a".repeat(40),
            after: "b".repeat(40),
            sender_id: id(1_234_567)?,
            forced: true,
        },
        EventPayload::CommandRecorded {
            command: "waive".to_owned(),
            actor_id: id(1_234_567)?,
            comment_id: id(2_400_000_001)?,
        },
        EventPayload::CommandRejected {
            command: "override".to_owned(),
            actor_id: id(7_654_321)?,
            reason: "actor is not an approver".to_owned(),
        },
        EventPayload::CommandVoided {
            comment_id: id(2_400_000_001)?,
            reason: VoidReason::Hash,
        },
        EventPayload::RetryGranted {
            role: "security".to_owned(),
        },
    ])
}

/// One payload of every kind, every optional field filled.
fn samples() -> Result<Vec<EventPayload>, Box<dyn Error>> {
    let mut samples = run_samples();
    samples.extend(finding_and_command_samples()?);
    Ok(samples)
}

fn event(payload: EventPayload) -> Result<Event, Box<dyn Error>> {
    Ok(Event {
        schema: EventSchema::V1,
        job: "decide".to_owned(),
        idx: 0,
        ts: "2026-10-11T06:16:00Z".to_owned(),
        run_key: "run-42-1".to_owned(),
        run_uid: run_uid(18_342_009_871)?,
        stage: "S7".to_owned(),
        payload,
    })
}

/// A new kind fails here until it has a sample, so the schema checks below cover it.
#[test]
fn the_samples_hold_exactly_one_payload_of_every_kind() -> Result<(), Box<dyn Error>> {
    let samples = samples()?;

    let kinds: BTreeSet<&str> = samples.iter().map(|p| p.kind().code()).collect();

    let every: BTreeSet<&str> = EventKind::ALL.iter().map(|k| k.code()).collect();
    assert_eq!(kinds, every);
    assert_eq!(samples.len(), EventKind::ALL.len(), "one sample per kind");
    Ok(())
}

#[test]
fn an_event_of_every_kind_is_schema_valid_and_tagged_with_its_kind_code()
-> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    for payload in samples()? {
        let kind = payload.kind();
        let written = serde_json::to_vec(&event(payload)?)?;

        let read = validator
            .read(Contract::Event, &written)
            .map_err(|e| format!("{kind:?}: {e:?}"))?;

        assert_eq!(
            read.get("kind"),
            Some(&Value::from(kind.code())),
            "{kind:?}"
        );
    }
    Ok(())
}

#[test]
fn an_event_of_every_kind_reads_back_as_written() -> Result<(), Box<dyn Error>> {
    for payload in samples()? {
        let written = event(payload)?;

        let read: Event = serde_json::from_slice(&serde_json::to_vec(&written)?)?;

        assert_eq!(read, written);
    }
    Ok(())
}

/// Rewrites each valid fixture of `contract` through its domain type with `rewrite`, and
/// validates the result.
fn rewritten_fixtures_validate(
    contract: Contract,
    rewrite: fn(&[u8]) -> serde_json::Result<Vec<u8>>,
) -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let fixtures = fixtures(contract, Validity::Valid)?;
    assert!(!fixtures.is_empty(), "no valid {contract} fixtures");

    for (name, bytes) in &fixtures {
        let written = rewrite(bytes).map_err(|e| format!("{name}: {e}"))?;

        let read = validator.read(contract, &written);

        assert!(
            matches!(read, Ok(Value::Object(_))),
            "{name}: rewritten document rejected: {read:?}"
        );
    }
    Ok(())
}

#[test]
fn every_findings_fixture_rewritten_by_the_domain_is_schema_valid() -> Result<(), Box<dyn Error>> {
    rewritten_fixtures_validate(Contract::Findings, |bytes| {
        serde_json::to_vec(&serde_json::from_slice::<FindingsDocument>(bytes)?)
    })
}

#[test]
fn every_event_fixture_rewritten_by_the_domain_is_schema_valid() -> Result<(), Box<dyn Error>> {
    rewritten_fixtures_validate(Contract::Event, |bytes| {
        serde_json::to_vec(&serde_json::from_slice::<Event>(bytes)?)
    })
}
