//! `decide` rules 8b and 8c of `docs/architecture/04-pipeline.md` §5: a required role that did
//! not complete, a failed coordinator or an unavailable verifier degrades the LLM stage, which
//! fails a PR of a blocking class or policy and otherwise warns, never silently (04 §3, 06 §6).

use std::collections::BTreeSet;
use std::error::Error;

use assay_domain::{
    CallFailure, Conclusion, Decision, DecisionInputs, Deferral, DegradedReport, DegradedRole,
    LlmStage, OwnerApproval, PrClass, ReasonCode, RequiredRole, Severity, StageRole,
    SupportOutcome, Tier, VerifierVerdict, decide,
};
use serde_json::Value;

use crate::support::{clean_inputs, confirmed, full_trust_path, reason, rubric_v1};

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

fn reported(role: StageRole, reason: CallFailure) -> DegradedReport {
    DegradedReport { role, reason }
}

fn degraded(role: &str, reason: &str) -> DegradedRole {
    DegradedRole {
        role: role.to_owned(),
        reason: reason.to_owned(),
    }
}

#[test]
fn a_lite_pr_whose_required_role_never_completed_warns_llm_degraded() -> Result<(), Box<dyn Error>>
{
    let mut inputs = clean_inputs();
    inputs.completed_roles = BTreeSet::from([RequiredRole::Bugs]);

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "combined: missing")]
    );
    assert_eq!(decision.degraded, [degraded("combined", "missing")]);
    Ok(())
}

#[test]
fn a_full_trust_path_pr_missing_meta_fails_required_role_degraded() -> Result<(), Box<dyn Error>> {
    let mut inputs = full_trust_path();
    inputs.completed_roles = BTreeSet::from([
        RequiredRole::Bugs,
        RequiredRole::Security,
        RequiredRole::Design,
    ]);

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::RequiredRoleDegraded, "meta: missing")]
    );
    assert_eq!(decision.degraded, [degraded("meta", "missing")]);
    Ok(())
}

#[test]
fn a_role_completed_on_one_shard_and_degraded_on_another_is_degraded_once()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.completed_roles = BTreeSet::from([RequiredRole::Combined]);
    inputs.degraded = vec![reported(StageRole::Combined, CallFailure::Cap)];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "combined: cap")]
    );
    assert_eq!(decision.degraded, [degraded("combined", "cap")]);
    Ok(())
}

#[test]
fn a_required_role_reported_degraded_is_not_also_missing() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.completed_roles = BTreeSet::new();
    inputs.degraded = vec![reported(StageRole::Combined, CallFailure::Turns)];

    let decision = decided(&inputs)?;

    assert_eq!(decision.degraded, [degraded("combined", "turns")]);
    Ok(())
}

#[test]
fn a_support_call_reported_degraded_is_not_also_missing() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.coordinator = SupportOutcome::NotNeeded;
    inputs.verifier = SupportOutcome::NotNeeded;
    inputs.findings = vec![confirmed("00000000000000b1", Severity::Blocker)];
    inputs.degraded = vec![
        reported(StageRole::Coordinator, CallFailure::Cap),
        reported(StageRole::Verifier, CallFailure::Turns),
    ];

    let decision = decided(&inputs)?;

    assert_eq!(
        decision.degraded,
        [
            degraded("coordinator", "cap"),
            degraded("verifier", "turns")
        ]
    );
    Ok(())
}

#[test]
fn a_failed_coordinator_fails_an_exec_surface_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = full_trust_path();
    inputs.pr.classes = vec![PrClass::ExecSurface];
    inputs.coordinator = SupportOutcome::Failed {
        reason: CallFailure::Timeout,
    };

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::RequiredRoleDegraded,
            "coordinator: timeout"
        )]
    );
    assert_eq!(decision.degraded, [degraded("coordinator", "timeout")]);
    Ok(())
}

#[test]
fn an_unavailable_verifier_warns_llm_degraded_on_an_unclassed_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.verifier = SupportOutcome::Failed {
        reason: CallFailure::Unavailable,
    };

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "verifier: unavailable")]
    );
    assert_eq!(decision.degraded, [degraded("verifier", "unavailable")]);
    Ok(())
}

#[test]
fn a_full_pr_whose_findings_no_coordinator_grouped_is_degraded() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.coordinator = SupportOutcome::NotNeeded;
    inputs.findings = vec![confirmed("00000000000000c1", Severity::Minor)];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "coordinator: missing")]
    );
    assert_eq!(decision.degraded, [degraded("coordinator", "missing")]);
    Ok(())
}

/// A ready PR on `tier` whose stage completed with one unverified blocker and no verifier run.
fn unverified_blocker_on(tier: Tier) -> DecisionInputs {
    let mut inputs = clean_inputs();
    inputs.pr.tier = tier;
    inputs.verifier = SupportOutcome::NotNeeded;
    let mut blocker = confirmed("00000000000000b1", Severity::Blocker);
    blocker.verdict = None;
    inputs.findings = vec![blocker];
    inputs
}

#[test]
fn a_full_candidate_blocker_no_verifier_ran_on_is_verifier_missing() -> Result<(), Box<dyn Error>> {
    let decision = decided(&unverified_blocker_on(Tier::Full))?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "verifier: missing")]
    );
    assert_eq!(decision.degraded, [degraded("verifier", "missing")]);
    Ok(())
}

#[test]
fn a_lite_candidate_major_no_verifier_ran_on_is_verifier_missing() -> Result<(), Box<dyn Error>> {
    let mut inputs = unverified_blocker_on(Tier::Lite);
    let mut major = confirmed("00000000000000a1", Severity::Major);
    major.verdict = None;
    inputs.findings = vec![major];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "verifier: missing")]
    );
    assert_eq!(decision.degraded, [degraded("verifier", "missing")]);
    Ok(())
}

#[test]
fn a_micro_pr_has_no_verifier_to_miss() -> Result<(), Box<dyn Error>> {
    let decision = decided(&unverified_blocker_on(Tier::Micro))?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    assert_eq!(decision.degraded, []);
    Ok(())
}

#[test]
fn minor_findings_need_no_verifier() -> Result<(), Box<dyn Error>> {
    let mut inputs = unverified_blocker_on(Tier::Lite);
    let mut minor = confirmed("00000000000000d1", Severity::Minor);
    minor.verdict = Some(VerifierVerdict::Refuted);
    inputs.findings = vec![minor];

    let decision = decided(&inputs)?;

    assert_eq!(decision.degraded, []);
    Ok(())
}

#[test]
fn a_role_degraded_for_two_reasons_is_listed_once_with_both() -> Result<(), Box<dyn Error>> {
    let mut inputs = full_trust_path();
    inputs.degraded = vec![
        reported(StageRole::Bugs, CallFailure::Timeout),
        reported(StageRole::Bugs, CallFailure::Cap),
    ];

    let decision = decided(&inputs)?;

    assert_eq!(decision.degraded, [degraded("bugs", "cap, timeout")]);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::RequiredRoleDegraded,
            "bugs: cap, timeout"
        )]
    );
    Ok(())
}

#[test]
fn a_support_call_reported_degraded_and_failed_is_listed_once() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.verifier = SupportOutcome::Failed {
        reason: CallFailure::Turns,
    };
    inputs.degraded = vec![reported(StageRole::Verifier, CallFailure::Turns)];

    let decision = decided(&inputs)?;

    assert_eq!(decision.degraded, [degraded("verifier", "turns")]);
    Ok(())
}

#[test]
fn the_inputs_digest_ignores_the_order_and_repetition_of_completed_roles()
-> Result<(), Box<dyn Error>> {
    let inputs = clean_inputs();
    let mut json = serde_json::to_value(&inputs)?;
    let roles = json
        .get_mut("completed_roles")
        .and_then(Value::as_array_mut)
        .ok_or("completed_roles is an array")?;
    roles.reverse();
    let first = roles.first().cloned().ok_or("a role")?;
    roles.push(first);
    let reordered: DecisionInputs = serde_json::from_value(json)?;

    let digest = decided(&reordered)?.inputs_digest;

    assert_eq!(digest, decided(&inputs)?.inputs_digest);
    Ok(())
}

#[test]
fn a_full_pr_without_findings_needs_no_coordinator() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.coordinator = SupportOutcome::NotNeeded;
    inputs.verifier = SupportOutcome::NotNeeded;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    assert_eq!(decision.degraded, []);
    Ok(())
}

#[test]
fn a_deferred_stage_reports_its_deferral_and_no_missing_role() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.llm = LlmStage::Deferred {
        reason: Deferral::BudgetDeferred,
    };
    inputs.completed_roles = BTreeSet::new();
    inputs.coordinator = SupportOutcome::NotNeeded;
    inputs.verifier = SupportOutcome::NotNeeded;

    let decision = decided(&inputs)?;

    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "budget-deferred")]
    );
    assert_eq!(decision.degraded, []);
    Ok(())
}

#[test]
fn degraded_roles_are_listed_sorted_and_once() -> Result<(), Box<dyn Error>> {
    let mut inputs = full_trust_path();
    inputs.pr.owner_approval = OwnerApproval::Approved;
    inputs.completed_roles = BTreeSet::from([RequiredRole::Bugs, RequiredRole::Design]);
    inputs.verifier = SupportOutcome::Failed {
        reason: CallFailure::Cap,
    };
    inputs.degraded = vec![
        reported(StageRole::Security, CallFailure::Timeout),
        reported(StageRole::Bugs, CallFailure::Cap),
        reported(StageRole::Security, CallFailure::Timeout),
    ];

    let decision = decided(&inputs)?;

    let listed = [
        degraded("bugs", "cap"),
        degraded("meta", "missing"),
        degraded("security", "timeout"),
        degraded("verifier", "cap"),
    ];
    assert_eq!(decision.degraded, listed);
    assert_eq!(
        decision.reasons,
        listed.map(|role| reason(
            ReasonCode::RequiredRoleDegraded,
            &format!("{}: {}", role.role, role.reason)
        ))
    );
    Ok(())
}

#[test]
fn the_inputs_digest_changes_with_each_role_run() -> Result<(), Box<dyn Error>> {
    let base = decided(&clean_inputs())?.inputs_digest;
    let mut fewer_roles = clean_inputs();
    fewer_roles.completed_roles.pop_last();
    let mut no_coordinator = clean_inputs();
    no_coordinator.coordinator = SupportOutcome::NotNeeded;
    let mut failed_verifier = clean_inputs();
    failed_verifier.verifier = SupportOutcome::Failed {
        reason: CallFailure::Cap,
    };

    let digests = [fewer_roles, no_coordinator, failed_verifier]
        .iter()
        .map(|inputs| decided(inputs).map(|d| d.inputs_digest))
        .collect::<Result<Vec<_>, _>>()?;

    assert!(digests.iter().all(|d| *d != base), "{base} in {digests:?}");
    Ok(())
}

/// `inputs` as JSON with `role` reported degraded for `reason`, parsed back.
fn reported_as(
    role: &str,
    reason: &str,
) -> Result<serde_json::Result<DecisionInputs>, Box<dyn Error>> {
    let mut json = serde_json::to_value(clean_inputs())?;
    *json
        .get_mut("degraded")
        .ok_or("the inputs carry `degraded`")? =
        serde_json::json!([{ "role": role, "reason": reason }]);
    Ok(serde_json::from_value(json))
}

/// `inputs` as JSON with one role reported degraded for `reason`, parsed back.
fn reported_for(reason: &str) -> Result<serde_json::Result<DecisionInputs>, Box<dyn Error>> {
    reported_as("bugs", reason)
}

/// Asserts that `parsed` is a data error whose message holds `fragment`.
fn assert_refused_with(parsed: &serde_json::Result<DecisionInputs>, fragment: &str) {
    assert!(
        matches!(parsed, Err(e) if e.is_data() && e.to_string().contains(fragment)),
        "expected a data error holding {fragment:?}, got {parsed:?}"
    );
}

#[test]
fn a_role_reported_degraded_with_an_empty_reason_is_refused() -> Result<(), Box<dyn Error>> {
    let parsed = reported_for("")?;

    assert_refused_with(&parsed, "unknown variant ``, expected one of `cap`");
    Ok(())
}

#[test]
fn a_role_reported_degraded_with_an_unknown_reason_is_refused() -> Result<(), Box<dyn Error>> {
    let parsed = reported_for("tired")?;

    assert_refused_with(&parsed, "unknown variant `tired`, expected one of `cap`");
    Ok(())
}

// #259 F1: the role is typed like the reason, so `decision-1`'s non-empty `degraded[].role`
// cannot be broken by a report the inputs carried.
#[test]
fn a_degraded_report_with_an_empty_role_is_refused() -> Result<(), Box<dyn Error>> {
    let parsed = reported_as("", "cap")?;

    assert_refused_with(&parsed, "unknown variant ``, expected one of `bugs`");
    Ok(())
}

#[test]
fn a_degraded_report_with_a_role_outside_the_stage_is_refused() -> Result<(), Box<dyn Error>> {
    let parsed = reported_as("auditor", "cap")?;

    assert_refused_with(&parsed, "unknown variant `auditor`, expected one of `bugs`");
    Ok(())
}

#[test]
fn a_degraded_report_reads_every_stage_role_as_its_code() -> Result<(), Box<dyn Error>> {
    for role in StageRole::ALL {
        let parsed = reported_as(role.code(), "cap")?.map(|inputs| {
            inputs
                .degraded
                .into_iter()
                .map(|d| d.role)
                .collect::<Vec<_>>()
        });

        assert_eq!(parsed.ok(), Some(vec![role]), "{}", role.code());
    }
    Ok(())
}

// #330 F1: `missing()` publishes a required role through `StageRole::code()`, so converting a
// role must keep its code.
#[test]
fn every_required_role_keeps_its_code_as_a_stage_role() {
    for role in RequiredRole::ALL {
        assert_eq!(StageRole::from(role).code(), role.code(), "{role:?}");
    }
}

#[test]
fn a_role_reported_degraded_with_a_call_failure_code_parses() -> Result<(), Box<dyn Error>> {
    let parsed = reported_for("cap")?;

    assert!(parsed.is_ok(), "a known reason was refused: {parsed:?}");
    Ok(())
}
