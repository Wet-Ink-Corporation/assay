//! The coordinator of #96 (04 §4 step 7) over recorded transcripts: Z3's validation keeps two
//! findings, `Replay` answers the coordinator's calls, and an accepted grouping or the
//! mechanical fallback follows.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    BundleFiles, Change, ContractValidator, CoordinateRequest, Coordination, CoordinationRefusal,
    DiffPaths, EgressScan, FileDiff, Hunk, Inputs, KnownRuns, LineSpan, MergedBy, PromptBundle,
    SuppressRules, ValidFinding, coordinate, validate,
};
use assay_domain::{Aeu, CallFailure, EventPayload, Finding, SupportOutcome};
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

/// Z3's validation of a `bugs` major on line 2 and a minor on line 1 of a new `src/lib.rs`.
fn validated() -> Result<Vec<ValidFinding>, Box<dyn Error>> {
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
        "source": {"kind": "reviewer", "name": "claude", "version": "v1", "role": "bugs",
                   "run_key": "run-96-1"},
        "findings": [{
            "class": "error-handling", "kind": "defect", "severity": "major", "file": LIB,
            "line": 2, "evidence": "let x = input.parse().unwrap();",
            "trigger": "a non-numeric request body reaches line 2 through parse",
            "title": "unwrap on user input panics", "body": "The handler panics."
        }, {
            "class": "error-handling", "kind": "defect", "severity": "minor", "file": LIB,
            "line": 1, "evidence": "pub fn parse(input: &str) -> Result<u32, Error> {",
            "title": "the parse error type is lost", "body": "The caller cannot tell why."
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
    let validated = validate(&ContractValidator::new()?, &inputs, &[document.as_bytes()]);
    if validated.findings.len() != 2 {
        return Err(format!("Z3 dropped a finding: {:?}", validated.transitions).into());
    }
    Ok(validated.findings)
}

/// Coordinates the validated findings against the recorded `transcripts`, in order.
fn coordinate_with(
    recordings: Vec<Recording>,
) -> Result<Coordination<ReplayError>, Box<dyn Error>> {
    let findings = validated()?;
    let bundle = PromptBundle::new(BundleFiles {
        system: "SYSTEM".to_owned(),
        bugs: "BUGS".to_owned(),
        security: "SECURITY".to_owned(),
        design: "DESIGN".to_owned(),
        meta: "META".to_owned(),
        combined: "COMBINED".to_owned(),
        combined_micro: "MICRO".to_owned(),
        verifier: "VERIFIER".to_owned(),
        coordinator: "COORDINATOR".to_owned(),
    })?;
    let runner = Replay::new(recordings);
    Ok(coordinate(
        &runner,
        CoordinateRequest {
            findings: &findings,
            bundle: &bundle,
            system_prompt_file: PathBuf::from("/base/plugin/prompts/v1/system.md"),
            add_dir: PathBuf::from("/work/pr"),
            timeout: Duration::from_secs(300),
        },
    ))
}

/// The recorded, completed stream `name`.
fn completed(name: &str) -> Result<Recording, Box<dyn Error>> {
    Ok(Recording::Completed(stream(name)?))
}

/// Each kept finding's position and group, in rank order.
fn ranking(coordination: &Coordination<ReplayError>) -> Vec<(usize, String)> {
    coordination
        .ranked
        .iter()
        .map(|ranked| (ranked.index, ranked.group.to_string()))
        .collect()
}

#[test]
fn a_recorded_grouping_is_accepted() -> Result<(), Box<dyn Error>> {
    let coordination = coordinate_with(vec![completed("coordinator_grouped")?])?;

    // The major ranks first whatever the coordinator's order; both share one group.
    assert_eq!(
        ranking(&coordination),
        [(0, "g1".to_owned()), (1, "g1".to_owned())]
    );
    assert_eq!(
        (coordination.outcome(), coordination.cost),
        (SupportOutcome::Completed, Aeu::from_micros(45_600))
    );
    assert_eq!(
        coordination.events().collect::<Vec<_>>(),
        [EventPayload::CoordinatorFinished {}]
    );
    Ok(())
}

#[test]
fn a_recorded_refused_answer_twice_falls_back_to_the_mechanical_merge() -> Result<(), Box<dyn Error>>
{
    let coordination = coordinate_with(vec![
        completed("coordinator_missing")?,
        completed("coordinator_missing")?,
    ])?;

    assert_eq!(
        (coordination.merged_by, coordination.outcome()),
        (
            MergedBy::Mechanical,
            SupportOutcome::Failed {
                reason: CallFailure::Malformed
            }
        )
    );
    assert_eq!(
        ranking(&coordination),
        [(0, "g1".to_owned()), (1, "g2".to_owned())]
    );
    assert_eq!(
        coordination.refusals,
        [CoordinationRefusal::Missing, CoordinationRefusal::Missing]
    );
    assert_eq!(
        coordination.events().collect::<Vec<_>>(),
        [EventPayload::StageDegraded {
            stage: "coordinator".to_owned(),
            reason: "malformed".to_owned()
        }]
    );
    assert_eq!(coordination.cost, Aeu::from_micros(64_200));
    Ok(())
}

#[test]
fn a_recorded_refusal_then_a_grouping_completes() -> Result<(), Box<dyn Error>> {
    let coordination = coordinate_with(vec![
        completed("coordinator_missing")?,
        completed("coordinator_grouped")?,
    ])?;

    assert_eq!(
        (coordination.outcome(), coordination.attempts),
        (SupportOutcome::Completed, 2)
    );
    Ok(())
}

#[test]
fn a_recorded_turn_limit_falls_back_without_a_retry() -> Result<(), Box<dyn Error>> {
    let coordination = coordinate_with(vec![
        completed("turns_exhausted")?,
        completed("coordinator_grouped")?,
    ])?;

    assert_eq!(
        (
            coordination.outcome(),
            coordination.attempts,
            coordination.merged_by
        ),
        (
            SupportOutcome::Failed {
                reason: CallFailure::Turns
            },
            1,
            MergedBy::Mechanical
        )
    );
    Ok(())
}

#[test]
fn a_recorded_timeout_falls_back_as_timeout() -> Result<(), Box<dyn Error>> {
    let coordination = coordinate_with(vec![Recording::TimedOut(stream("partial")?)])?;

    assert_eq!(
        coordination.outcome(),
        SupportOutcome::Failed {
            reason: CallFailure::Timeout
        }
    );
    assert_eq!(coordination.ranked.len(), 2);
    Ok(())
}
