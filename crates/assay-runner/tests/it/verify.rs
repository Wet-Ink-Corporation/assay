//! The blind verifier of #91 (04 §4 step 8) over recorded verifier transcripts: Z3's validation
//! keeps a finding, `Replay` answers its verifier call, and the verdict, severity, events and
//! reject rate follow.

use std::collections::BTreeMap;
use std::error::Error;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    BundleFiles, Change, ContractValidator, DiffPaths, EgressScan, FileDiff, Hunk, Inputs,
    KnownRuns, LineSpan, PromptBundle, ReviewerDocument, Role, SuppressRules, ValidFinding,
    VerdictTally, Verification, VerifiedFinding, VerifierStatus, VerifierTier, VerifyRequest,
    validate, verify,
};
use assay_domain::{
    Aeu, CallFailure, Disposition, Event, EventPayload, EventSchema, Finding, RunUid, Severity,
    SupportOutcome, VerifierVerdict,
};
use assay_runner::{Recording, Replay, ReplayError};
use serde_json::json;

use crate::support::stream;

const REPOSITORY: &str = "Wet-Ink-Corporation/assay";

const LIB: &str = "src/lib.rs";

const LIB_AT_HEAD: &str = "\
pub fn parse(input: &str) -> Result<u32, Error> {
    let x = input.parse().unwrap();
    Ok(x)
}
";

struct NoRules;

impl SuppressRules for NoRules {
    fn suppresses(&self, _finding: &Finding, _file_at_head: &str) -> bool {
        false
    }
}

/// Z3's validation of one `combined` major on line 2 of a new `src/lib.rs`.
fn validated_major() -> Result<Vec<ValidFinding>, Box<dyn Error>> {
    let change = Change {
        head_files: BTreeMap::from([(LIB.to_owned(), LIB_AT_HEAD.to_owned())]),
        base_files: BTreeMap::new(),
        diffs: vec![FileDiff {
            paths: DiffPaths::Added {
                head: LIB.to_owned(),
            },
            hunks: vec![Hunk {
                base: LineSpan { start: 0, count: 0 },
                head: LineSpan { start: 1, count: 4 },
            }],
        }],
    };
    let document = json!({
        "schema": "assay.findings/1",
        "source": {"kind": "reviewer", "name": "claude", "version": "v1", "role": "combined",
                   "run_key": "run-91-1"},
        "findings": [{
            "class": "error-handling", "kind": "defect", "severity": "major", "file": LIB,
            "line": 2, "evidence": "let x = input.parse().unwrap();",
            "trigger": "a non-numeric request body reaches line 2 through parse",
            "title": "unwrap on user input panics", "body": "The handler panics."
        }]
    })
    .to_string();
    let egress = EgressScan::without_token(KnownRuns::index(
        change.head_files.values().map(String::as_bytes),
    )?);
    let inputs = Inputs {
        repository: REPOSITORY,
        change: &change,
        suppress: &NoRules,
        dismissals: &[],
        gate_findings: &[],
        egress: &egress,
    };
    let combined = ReviewerDocument {
        role: Role::Combined,
        bytes: document.as_bytes(),
    };
    let validated = validate(&ContractValidator::new()?, &inputs, &[combined]);
    Ok(validated.findings)
}

/// Verifies the validated major in `lite` against the recorded `transcripts`, in order.
fn verify_with(transcripts: &[&str]) -> Result<Verification<ReplayError>, Box<dyn Error>> {
    let findings = validated_major()?;
    let bundle = PromptBundle::new(BundleFiles {
        system: "SYSTEM".to_owned(),
        bugs: "BUGS".to_owned(),
        security: "SECURITY".to_owned(),
        design: "DESIGN".to_owned(),
        meta: "META".to_owned(),
        combined: "COMBINED".to_owned(),
        combined_micro: "MICRO".to_owned(),
        verifier: "VERIFIER".to_owned(),
        verifier_carried: "VERIFIER-CARRIED".to_owned(),
        coordinator: "COORDINATOR".to_owned(),
    })?;
    let recordings = transcripts
        .iter()
        .map(|name| Ok(Recording::Completed(stream(name)?)))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let runner = Replay::new(recordings);
    Ok(verify(
        &runner,
        VerifyRequest {
            tier: VerifierTier::Lite,
            repository: REPOSITORY,
            findings: &findings,
            carried: &[],
            bundle: &bundle,
            system_prompt_file: PathBuf::from("/base/plugin/prompts/v1/system.md"),
            add_dir: PathBuf::from("/work/pr"),
            timeout: Duration::from_secs(300),
        },
    ))
}

/// The one verified finding's severity before and after, and its verdict and reason.
fn judged(
    verification: &Verification<ReplayError>,
) -> Option<(Severity, Severity, VerifierVerdict, &str)> {
    let [found] = verification.findings.as_slice() else {
        return None;
    };
    let VerifierStatus::Judged(judgement) = &found.status else {
        return None;
    };
    Some((
        found.raised,
        found.severity,
        judgement.verdict(),
        judgement.reason().as_str(),
    ))
}

#[test]
fn a_recorded_confirmation_keeps_the_major() -> Result<(), Box<dyn Error>> {
    let verification = verify_with(&["verifier_confirmed"])?;

    assert_eq!(
        judged(&verification),
        Some((
            Severity::Major,
            Severity::Major,
            VerifierVerdict::Confirmed,
            "parse runs on the request body unchecked, so a non-numeric body panics"
        ))
    );
    assert_eq!(verification.outcome(), SupportOutcome::Completed);
    assert_eq!(
        verification.events().collect::<Vec<_>>(),
        [EventPayload::FindingVerified {
            verdict: VerifierVerdict::Confirmed
        }]
    );
    assert_eq!(verification.cost, Aeu::from_micros(12_300));
    Ok(())
}

#[test]
fn a_recorded_refutation_downgrades_to_minor_with_the_reason() -> Result<(), Box<dyn Error>> {
    let verification = verify_with(&["verifier_refuted"])?;
    let validated_fp = validated_major()?
        .first()
        .map(|found| found.fingerprint)
        .ok_or("the fixture validates one major")?;

    assert_eq!(
        judged(&verification),
        Some((
            Severity::Major,
            Severity::Minor,
            VerifierVerdict::Refuted,
            "the only caller validates the input as digits before parse"
        ))
    );
    assert_eq!(
        verification.events().collect::<Vec<_>>(),
        [
            EventPayload::FindingVerified {
                verdict: VerifierVerdict::Refuted
            },
            EventPayload::FindingDisposed {
                code: Disposition::VerifierRejectedDowngraded,
                fp: Some(validated_fp),
                from: Some(Severity::Major),
                to: Some(Severity::Minor),
            },
        ]
    );
    Ok(())
}

#[test]
fn a_recorded_unclear_verdict_goes_one_level_down() -> Result<(), Box<dyn Error>> {
    let verification = verify_with(&["verifier_unclear"])?;

    assert_eq!(
        judged(&verification),
        Some((
            Severity::Major,
            Severity::Minor,
            VerifierVerdict::Unclear,
            "no caller of parse was found within the turn limit"
        ))
    );
    assert_eq!(
        verification.events().collect::<Vec<_>>(),
        [EventPayload::FindingVerified {
            verdict: VerifierVerdict::Unclear
        }]
    );
    Ok(())
}

#[test]
fn a_recorded_bad_verdict_is_retried_then_degrades_the_verifier() -> Result<(), Box<dyn Error>> {
    let verification = verify_with(&["verifier_bad_verdict", "verifier_bad_verdict"])?;

    assert_eq!(
        verification.outcome(),
        SupportOutcome::Failed {
            reason: CallFailure::Malformed
        }
    );
    let shown: Vec<_> = verification
        .findings
        .iter()
        .map(|found| (found.severity, found.status.clone()))
        .collect();
    assert_eq!(shown, [(Severity::Major, VerifierStatus::Unverified)]);
    assert_eq!(
        (verification.attempts, verification.cost),
        (2, Aeu::from_micros(22_200))
    );
    Ok(())
}

#[test]
fn a_bad_verdict_then_a_good_one_is_judged() -> Result<(), Box<dyn Error>> {
    let verification = verify_with(&["verifier_bad_verdict", "verifier_refuted"])?;

    assert_eq!(
        verification
            .findings
            .first()
            .and_then(VerifiedFinding::verdict),
        Some(VerifierVerdict::Refuted)
    );
    Ok(())
}

#[test]
fn the_reject_rate_is_computed_from_the_emitted_events() -> Result<(), Box<dyn Error>> {
    let runs = [
        "verifier_refuted",
        "verifier_confirmed",
        "verifier_unclear",
        "verifier_confirmed",
    ];
    let mut log = Vec::new();
    for (run_id, transcript) in (1..).zip(runs) {
        let verification = verify_with(&[transcript])?;
        for payload in verification.events() {
            log.push(Event {
                schema: EventSchema::V1,
                job: "review".to_owned(),
                idx: 0,
                ts: "2026-10-13T00:00:00Z".to_owned(),
                run_key: "run".to_owned(),
                run_uid: RunUid {
                    repo_id: NonZeroU64::MIN,
                    run_id: NonZeroU64::new(run_id).ok_or("run ids start at 1")?,
                    run_attempt: NonZeroU64::MIN,
                },
                stage: "S6".to_owned(),
                payload,
            });
        }
    }

    let tally = VerdictTally::of(&log);

    assert_eq!(
        (
            tally.refuted(),
            tally.confirmed() + tally.refuted() + tally.unclear(),
            tally.runs()
        ),
        (1, 4, 4)
    );
    Ok(())
}
