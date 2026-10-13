//! The coordinator (S5, 04 §3, §4 step 7, #96): one call that groups, merges and marks noise in
//! finding ids, read strictly, with the mechanical merge standing in whenever no answer is
//! accepted.

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    COORDINATION_SCHEMA, COORDINATOR_CAP, COORDINATOR_MAX_TURNS, CoordinateRequest, Coordination,
    CoordinationRefusal, CoordinatorFailure, MAX_COORDINATED_FINDINGS, Merged, MergedBy,
    ModelOutcome, Placement, PromptBundle, ValidFinding, coordinate, merge_mechanically,
};
use assay_domain::{
    Aeu, CallFailure, Confidence, Disposition, EventPayload, Severity, SupportOutcome,
};
use serde_json::{Value, json};

use crate::verify_fakes::{Scripted, Unreachable, bundle, finding, output};

/// A finding of `severity` with its own fingerprint `fp`, titled `finding <fp>`.
fn distinct(fp: u64, severity: Severity) -> Result<ValidFinding, Box<dyn Error>> {
    let mut found = finding(0, severity)?;
    found.fingerprint = format!("{fp:016x}").parse()?;
    found.finding.title = format!("finding {fp}");
    found.finding.confidence = None;
    Ok(found)
}

/// Findings of each severity in order, fingerprints 1, 2, ...
fn findings(severities: &[Severity]) -> Result<Vec<ValidFinding>, Box<dyn Error>> {
    (1..)
        .zip(severities)
        .map(|(fp, &severity)| distinct(fp, severity))
        .collect()
}

fn request<'a>(findings: &'a [ValidFinding], bundle: &'a PromptBundle) -> CoordinateRequest<'a> {
    CoordinateRequest {
        findings,
        bundle,
        system_prompt_file: PathBuf::from("/base/plugin/prompts/v1/system.md"),
        add_dir: PathBuf::from("/work/pr"),
        timeout: Duration::from_secs(300),
    }
}

fn run(
    runner: &Scripted,
    findings: &[ValidFinding],
) -> Result<Coordination<Unreachable>, Box<dyn Error>> {
    let bundle = bundle()?;
    Ok(coordinate(runner, request(findings, &bundle)))
}

/// Each kept finding's position and group, in rank order.
fn ranking<E>(coordination: &Coordination<E>) -> Vec<(usize, String)> {
    coordination
        .ranked
        .iter()
        .map(|ranked| (ranked.index, ranked.group.to_string()))
        .collect()
}

/// An answer of `groups` (members only) and `noise`.
fn answer(groups: &[&[u64]], noise: &[u64]) -> ModelOutcome {
    let groups: Vec<Value> = groups
        .iter()
        .map(|members| json!({"findings": members, "duplicates": []}))
        .collect();
    output(json!({"groups": groups, "noise": noise}))
}

/// The JSON object each call showed the coordinator, in order.
fn shown(runner: &Scripted) -> Vec<Value> {
    runner
        .calls
        .borrow()
        .iter()
        .map(|call| {
            call.user_prompt
                .as_str()
                .split_once("\n\n")
                .and_then(|(shown, _)| serde_json::from_str(shown).ok())
                .unwrap_or(Value::Null)
        })
        .collect()
}

#[test]
fn no_findings_make_no_call_and_need_no_coordinator() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::answering([]);

    let coordination = run(&runner, &[])?;

    assert_eq!(runner.call_count(), 0);
    assert_eq!(coordination.outcome(), SupportOutcome::NotNeeded);
    assert_eq!(coordination.events().count(), 0);
    Ok(())
}

#[test]
fn an_accepted_answer_groups_merges_and_ranks_the_findings() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[
        Severity::Minor,
        Severity::Major,
        Severity::Major,
        Severity::Minor,
    ])?;
    // 1 and 2 share a root cause, 3 duplicates 0, and the coordinator lists the minor first.
    let runner = Scripted::answering([(
        output(json!({
            "groups": [{"findings": [0], "duplicates": [{"id": 3, "of": 0}]},
                       {"findings": [2, 1], "duplicates": []}],
            "noise": []
        })),
        40_000,
    )]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        ranking(&coordination),
        [
            (2, "g1".to_owned()),
            (1, "g1".to_owned()),
            (0, "g2".to_owned())
        ]
    );
    assert_eq!(coordination.merged, [Merged { index: 3, into: 0 }]);
    assert_eq!(
        (coordination.merged_by, coordination.outcome()),
        (MergedBy::Coordinator, SupportOutcome::Completed)
    );
    assert_eq!(
        coordination.events().collect::<Vec<_>>(),
        [
            EventPayload::FindingDisposed {
                code: Disposition::Duplicate,
                from: None,
                to: None
            },
            EventPayload::CoordinatorFinished {},
        ]
    );
    assert_eq!(
        (coordination.attempts, coordination.cost),
        (1, Aeu::from_micros(40_000))
    );
    Ok(())
}

#[test]
fn the_coordinator_is_shown_only_the_listed_fields_under_its_own_limits()
-> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major])?;
    let runner = Scripted::answering([(answer(&[&[0]], &[]), 10_000)]);

    run(&runner, &findings)?;

    let calls = runner.calls.borrow();
    let call = calls.first().ok_or("one call")?;
    assert_eq!(
        (call.json_schema.as_str(), call.max_turns, call.budget),
        (COORDINATION_SCHEMA, COORDINATOR_MAX_TURNS, COORDINATOR_CAP)
    );
    assert!(call.user_prompt.as_str().ends_with("\n\nCOORDINATOR"));
    let shown = shown(&runner);
    assert_eq!(
        shown,
        [json!({"findings": [{
            "id": 0, "class": "error-handling", "severity": "major", "confidence": null,
            "pre_existing": false, "file": "src/lib.rs", "line": 12, "title": "finding 1",
            "evidence": "let x = input.parse().unwrap();",
            "trigger": "a non-numeric input reaches line 12 through parse",
            "group_hint": "GROUP the verifier must not see"
        }]})]
    );
    Ok(())
}

#[test]
fn findings_of_one_fingerprint_are_merged_before_the_call() -> Result<(), Box<dyn Error>> {
    let mut findings = findings(&[Severity::Minor, Severity::Major])?;
    findings.push(distinct(1, Severity::Major)?);
    let runner = Scripted::answering([(answer(&[&[0], &[1]], &[]), 10_000)]);

    let coordination = run(&runner, &findings)?;

    // The minor 0 is merged into the major 2 of its fingerprint; the coordinator sees 1 and 2.
    let shown = shown(&runner);
    let titles: Vec<&str> = shown
        .iter()
        .filter_map(|shown| shown["findings"].as_array())
        .flatten()
        .filter_map(|found| found["title"].as_str())
        .collect();
    assert_eq!(titles, ["finding 2", "finding 1"]);
    assert_eq!(coordination.merged, [Merged { index: 0, into: 2 }]);
    assert_eq!(
        ranking(&coordination),
        [(1, "g1".to_owned()), (2, "g2".to_owned())]
    );
    Ok(())
}

#[test]
fn a_finding_merged_before_the_call_follows_its_keeper_into_the_coordinators_merge()
-> Result<(), Box<dyn Error>> {
    let mut findings = findings(&[Severity::Minor, Severity::Major])?;
    findings.push(distinct(1, Severity::Major)?);
    // Shown: 0 is finding 1, 1 is finding 2, which the coordinator merges into finding 1.
    let runner = Scripted::answering([(
        output(json!({
            "groups": [{"findings": [0], "duplicates": [{"id": 1, "of": 0}]}],
            "noise": []
        })),
        10_000,
    )]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        coordination.merged,
        [Merged { index: 0, into: 1 }, Merged { index: 2, into: 1 }]
    );
    assert_eq!(ranking(&coordination), [(1, "g1".to_owned())]);
    Ok(())
}

#[test]
fn noise_moves_a_minor_to_the_summary_and_ranks_it_last() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Minor, Severity::Nit])?;
    let runner = Scripted::answering([(answer(&[&[0], &[1]], &[0]), 10_000)]);

    let coordination = run(&runner, &findings)?;

    let placed: Vec<_> = coordination
        .ranked
        .iter()
        .map(|ranked| (ranked.index, ranked.placement))
        .collect();
    assert_eq!(placed, [(1, Placement::Inline), (0, Placement::Summary)]);
    Ok(())
}

#[test]
fn the_rank_is_severity_then_confidence_then_the_coordinators_order() -> Result<(), Box<dyn Error>>
{
    let mut findings = findings(&[Severity::Major, Severity::Major, Severity::Blocker])?;
    if let Some(found) = findings.get_mut(1) {
        found.finding.confidence = Some(Confidence::High);
    }
    let runner = Scripted::answering([(answer(&[&[0], &[1], &[2]], &[]), 10_000)]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        ranking(&coordination),
        [
            (2, "g1".to_owned()),
            (1, "g2".to_owned()),
            (0, "g3".to_owned())
        ]
    );
    Ok(())
}

#[test]
fn a_refused_answer_twice_falls_back_to_the_mechanical_merge() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major, Severity::Major])?;
    // Both answers leave finding 1 out of every group.
    let runner = Scripted::answering([
        (answer(&[&[0]], &[]), 30_000),
        (answer(&[&[0]], &[]), 20_000),
    ]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        coordination.refusals,
        [CoordinationRefusal::Missing, CoordinationRefusal::Missing]
    );
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
        coordination.events().collect::<Vec<_>>(),
        [EventPayload::StageDegraded {
            stage: "coordinator".to_owned(),
            reason: "malformed".to_owned()
        }]
    );
    assert_eq!(
        (coordination.attempts, coordination.cost),
        (2, Aeu::from_micros(50_000))
    );
    Ok(())
}

#[test]
fn a_refused_answer_then_an_accepted_one_completes() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major, Severity::Major])?;
    let runner = Scripted::answering([
        (answer(&[&[0, 0]], &[]), 30_000),
        (answer(&[&[1, 0]], &[]), 20_000),
    ]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(coordination.refusals, [CoordinationRefusal::Repeated]);
    assert_eq!(coordination.outcome(), SupportOutcome::Completed);
    assert_eq!(
        ranking(&coordination),
        [(1, "g1".to_owned()), (0, "g1".to_owned())]
    );
    Ok(())
}

#[test]
fn malformed_output_twice_falls_back_as_malformed() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major])?;
    let runner = Scripted::answering([
        (ModelOutcome::Malformed, 10_000),
        (ModelOutcome::Malformed, 10_000),
    ]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        (coordination.failure, coordination.attempts),
        (Some(CoordinatorFailure::Call(CallFailure::Malformed)), 2)
    );
    Ok(())
}

#[test]
fn a_call_that_ends_without_output_falls_back_with_its_reason_and_no_retry()
-> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major])?;
    let endings = [
        (ModelOutcome::BudgetExhausted, CallFailure::Cap),
        (ModelOutcome::TurnsExhausted, CallFailure::Turns),
        (ModelOutcome::Timeout, CallFailure::Timeout),
        (ModelOutcome::QuotaExhausted, CallFailure::Unavailable),
    ];

    for (ending, reason) in endings {
        let runner = Scripted::answering([(ending, 10_000)]);

        let coordination = run(&runner, &findings)?;

        assert_eq!(
            (
                coordination.outcome(),
                runner.call_count(),
                coordination.merged_by
            ),
            (SupportOutcome::Failed { reason }, 1, MergedBy::Mechanical),
            "{reason:?}"
        );
    }
    Ok(())
}

#[test]
fn a_runner_error_falls_back_as_unavailable_charged_at_the_budget() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major])?;
    let runner = Scripted::new([Err(Unreachable)]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        (
            coordination.outcome(),
            coordination.cost,
            coordination.failure
        ),
        (
            SupportOutcome::Failed {
                reason: CallFailure::Unavailable
            },
            COORDINATOR_CAP,
            Some(CoordinatorFailure::Runner(Unreachable))
        )
    );
    Ok(())
}

#[test]
fn a_retry_gets_only_what_is_left_of_the_cap() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major])?;
    let runner = Scripted::answering([
        (ModelOutcome::Malformed, 120_000),
        (answer(&[&[0]], &[]), 10_000),
    ]);

    run(&runner, &findings)?;

    let budgets: Vec<Aeu> = runner
        .calls
        .borrow()
        .iter()
        .map(|call| call.budget)
        .collect();
    assert_eq!(budgets, [COORDINATOR_CAP, Aeu::from_micros(180_000)]);
    Ok(())
}

#[test]
fn a_first_attempt_that_spends_the_cap_leaves_no_retry_and_fails_as_cap()
-> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Major])?;
    let runner = Scripted::answering([(answer(&[], &[]), COORDINATOR_CAP.micros())]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        (
            coordination.failure,
            coordination.attempts,
            coordination.refusals
        ),
        (
            Some(CoordinatorFailure::Call(CallFailure::Cap)),
            1,
            vec![CoordinationRefusal::Missing]
        )
    );
    Ok(())
}

#[test]
fn the_most_distinct_findings_a_call_takes_are_coordinated() -> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Minor; MAX_COORDINATED_FINDINGS])?;
    let ids: Vec<u64> = (0..).take(MAX_COORDINATED_FINDINGS).collect();
    let runner = Scripted::answering([(answer(&[&ids], &[]), 10_000)]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(
        (runner.call_count(), coordination.outcome()),
        (1, SupportOutcome::Completed)
    );
    Ok(())
}

#[test]
fn one_distinct_finding_more_than_a_call_takes_is_merged_without_a_call()
-> Result<(), Box<dyn Error>> {
    let findings = findings(&[Severity::Minor; MAX_COORDINATED_FINDINGS + 1])?;
    let runner = Scripted::answering([]);

    let coordination = run(&runner, &findings)?;

    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        (coordination.outcome(), coordination.failure),
        (
            SupportOutcome::Failed {
                reason: CallFailure::Cap
            },
            Some(CoordinatorFailure::TooMany {
                findings: MAX_COORDINATED_FINDINGS + 1
            })
        )
    );
    assert_eq!(coordination.ranked.len(), MAX_COORDINATED_FINDINGS + 1);
    Ok(())
}

#[test]
fn the_mechanical_merge_keeps_the_most_severe_copy_and_never_hides_a_new_finding()
-> Result<(), Box<dyn Error>> {
    let mut findings = findings(&[Severity::Minor])?;
    findings.push(distinct(1, Severity::Major)?);
    // The same fingerprint again, pre-existing: merged into 1, which is not.
    let mut old = distinct(1, Severity::Minor)?;
    old.finding.pre_existing = true;
    findings.push(old);
    // A new finding under a pre-existing blocker of its fingerprint stays its own.
    let mut old_blocker = distinct(2, Severity::Blocker)?;
    old_blocker.finding.pre_existing = true;
    findings.push(old_blocker);
    findings.push(distinct(2, Severity::Major)?);

    let coordination = merge_mechanically::<Unreachable>(&findings);

    assert_eq!(
        coordination.merged,
        [Merged { index: 0, into: 1 }, Merged { index: 2, into: 1 }]
    );
    assert_eq!(
        ranking(&coordination),
        [
            (3, "g1".to_owned()),
            (1, "g2".to_owned()),
            (4, "g3".to_owned())
        ]
    );
    Ok(())
}

#[test]
fn merging_mechanically_needs_no_coordinator_and_records_each_duplicate()
-> Result<(), Box<dyn Error>> {
    let mut findings = findings(&[Severity::Major])?;
    findings.push(distinct(1, Severity::Minor)?);

    let coordination = merge_mechanically::<Unreachable>(&findings);

    assert_eq!(coordination.outcome(), SupportOutcome::NotNeeded);
    assert_eq!(
        coordination.events().collect::<Vec<_>>(),
        [EventPayload::FindingDisposed {
            code: Disposition::Duplicate,
            from: None,
            to: None
        }]
    );
    Ok(())
}

#[test]
fn a_finding_placed_in_the_summary_stays_there() -> Result<(), Box<dyn Error>> {
    let mut findings = findings(&[Severity::Major])?;
    if let Some(found) = findings.first_mut() {
        found.placement = Placement::Summary;
    }
    let runner = Scripted::answering([(answer(&[&[0]], &[]), 10_000)]);

    let coordination = run(&runner, &findings)?;

    let placed: Vec<_> = coordination.ranked.iter().map(|r| r.placement).collect();
    assert_eq!(placed, [Placement::Summary]);
    Ok(())
}
