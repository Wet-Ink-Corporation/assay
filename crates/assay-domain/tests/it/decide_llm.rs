//! `decide` rules 6, 7 and 8 of `docs/architecture/04-pipeline.md` §5: verified blockers,
//! majors by root cause, and a deferred or degraded LLM stage.

use std::error::Error;

use assay_domain::{
    Authorship, CallFailure, Conclusion, Decision, DecisionInputs, Deferral, DegradedReport,
    DegradedRole, InfraPolicy, LlmPolicy, LlmStage, OwnerApproval, PrClass, Reason, ReasonCode,
    Severity, StageRole, VerifierVerdict, decide,
};

use crate::support::{clean_inputs, confirmed, fp, reason, rubric_v1};

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

fn blocking(mut inputs: DecisionInputs) -> DecisionInputs {
    inputs.policy.llm_blockers = LlmPolicy::Block;
    inputs.policy.llm_majors = LlmPolicy::Block;
    inputs
}

fn deferred(reason: Deferral) -> DecisionInputs {
    let mut inputs = clean_inputs();
    inputs.llm = LlmStage::Deferred { reason };
    inputs
}

fn trust_path(mut inputs: DecisionInputs) -> DecisionInputs {
    inputs.pr.classes = vec![PrClass::TrustPath];
    inputs.pr.owner_approval = OwnerApproval::Approved;
    inputs
}

fn bugs_reported() -> DegradedReport {
    DegradedReport {
        role: StageRole::Bugs,
        reason: CallFailure::Cap,
    }
}

fn bugs_degraded() -> DegradedRole {
    DegradedRole {
        role: "bugs".to_owned(),
        reason: "cap".to_owned(),
    }
}

#[test]
fn an_advisory_verified_blocker_succeeds_and_is_listed() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.findings = vec![confirmed("00000000000000b1", Severity::Blocker)];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    assert_eq!(decision.advisory_blockers, [fp("00000000000000b1")]);
    Ok(())
}

#[test]
fn a_verified_blocker_fails_when_llm_blockers_block() -> Result<(), Box<dyn Error>> {
    let mut inputs = blocking(clean_inputs());
    inputs.findings = vec![confirmed("00000000000000b1", Severity::Blocker)];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [Reason {
            code: ReasonCode::BlockerVerified,
            detail: "verified blocker".to_owned(),
            fp: Some(fp("00000000000000b1")),
        }]
    );
    assert_eq!(decision.advisory_blockers, []);
    Ok(())
}

#[test]
fn pre_existing_or_unconfirmed_blockers_never_fail() -> Result<(), Box<dyn Error>> {
    let mut inputs = blocking(clean_inputs());
    let mut pre_existing = confirmed("00000000000000b1", Severity::Blocker);
    pre_existing.pre_existing = true;
    let mut unclear = confirmed("00000000000000b2", Severity::Blocker);
    unclear.verdict = Some(VerifierVerdict::Unclear);
    let mut unverified = confirmed("00000000000000b3", Severity::Blocker);
    unverified.verdict = None;
    inputs.findings = vec![pre_existing, unclear, unverified];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn three_root_causes_of_confirmed_majors_fail_when_llm_majors_block() -> Result<(), Box<dyn Error>>
{
    let mut inputs = blocking(clean_inputs());
    inputs.findings = ["01", "02", "03"]
        .map(|n| confirmed(&format!("00000000000000{n}"), Severity::Major))
        .to_vec();

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::MajorsOverThreshold,
            "3 confirmed majors by root cause (threshold 3)"
        )]
    );
    Ok(())
}

#[test]
fn majors_of_one_root_cause_count_once() -> Result<(), Box<dyn Error>> {
    let mut inputs = blocking(clean_inputs());
    inputs.findings = ["01", "02", "03"]
        .map(|n| {
            let mut finding = confirmed(&format!("00000000000000{n}"), Severity::Major);
            finding.group_id = "one-root-cause".to_owned();
            finding
        })
        .to_vec();

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    Ok(())
}

#[test]
fn micro_unconfirmed_and_pre_existing_majors_are_not_counted() -> Result<(), Box<dyn Error>> {
    let mut inputs = blocking(clean_inputs());
    let mut micro = confirmed("0000000000000001", Severity::Major);
    micro.role = "combined-micro".to_owned();
    let mut refuted = confirmed("0000000000000002", Severity::Major);
    refuted.verdict = Some(VerifierVerdict::Refuted);
    let mut pre_existing = confirmed("0000000000000003", Severity::Major);
    pre_existing.pre_existing = true;
    inputs.findings = vec![
        micro,
        refuted,
        pre_existing,
        confirmed("0000000000000004", Severity::Major),
        confirmed("0000000000000005", Severity::Major),
    ];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    Ok(())
}

#[test]
fn advisory_majors_never_fail() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.findings = ["01", "02", "03", "04"]
        .map(|n| confirmed(&format!("00000000000000{n}"), Severity::Major))
        .to_vec();

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn a_budget_deferred_trust_path_pr_is_pending() -> Result<(), Box<dyn Error>> {
    let inputs = trust_path(deferred(Deferral::BudgetDeferred));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Pending);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::BudgetDeferred, "requeued 0 of 3 times")]
    );
    Ok(())
}

#[test]
fn an_unavailable_llm_is_pending_when_policy_blocks_on_infra() -> Result<(), Box<dyn Error>> {
    let mut inputs = deferred(Deferral::InfraLlmUnavailable);
    inputs.policy.on_infra_failure = InfraPolicy::Block;
    inputs.pr.requeues = 2;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Pending);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::InfraLlmUnavailable,
            "requeued 2 of 3 times"
        )]
    );
    Ok(())
}

#[test]
fn a_deferral_fails_infra_llm_unavailable_after_its_requeues() -> Result<(), Box<dyn Error>> {
    let mut inputs = trust_path(deferred(Deferral::BudgetDeferred));
    inputs.pr.requeues = 3;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::InfraLlmUnavailable,
            "budget-deferred after 3 requeues"
        )]
    );
    Ok(())
}

#[test]
fn a_busy_slot_holds_an_agent_authored_pr_pending() -> Result<(), Box<dyn Error>> {
    let mut inputs = deferred(Deferral::SlotBusy);
    inputs.pr.authorship = Authorship::Agent;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Pending);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::SlotBusy, "requeued 0 of 3 times")]
    );
    Ok(())
}

#[test]
fn an_agent_authored_budget_deferral_only_warns() -> Result<(), Box<dyn Error>> {
    let mut inputs = deferred(Deferral::BudgetDeferred);
    inputs.pr.authorship = Authorship::Agent;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "budget-deferred")]
    );
    Ok(())
}

#[test]
fn a_busy_slot_on_a_human_pr_warns_llm_degraded() -> Result<(), Box<dyn Error>> {
    let inputs = deferred(Deferral::SlotBusy);

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "slot-busy")]
    );
    Ok(())
}

#[test]
fn a_degraded_required_role_fails_a_trust_path_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = trust_path(clean_inputs());
    inputs.degraded = vec![bugs_reported()];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::RequiredRoleDegraded, "bugs: cap")]
    );
    assert_eq!(decision.degraded, [bugs_degraded()]);
    Ok(())
}

#[test]
fn a_degraded_required_role_otherwise_warns_llm_degraded() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.degraded = vec![bugs_reported()];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "bugs: cap")]
    );
    Ok(())
}

#[test]
fn a_missing_owner_approval_wins_over_a_pending_deferral() -> Result<(), Box<dyn Error>> {
    let mut inputs = trust_path(deferred(Deferral::SlotBusy));
    inputs.pr.owner_approval = OwnerApproval::NotApproved;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons.iter().map(|r| r.code).collect::<Vec<_>>(),
        [ReasonCode::TrustPathUnapproved, ReasonCode::SlotBusy]
    );
    Ok(())
}

#[test]
fn a_pending_deferral_wins_over_a_degraded_role() -> Result<(), Box<dyn Error>> {
    let mut inputs = trust_path(deferred(Deferral::BudgetDeferred));
    inputs.degraded = vec![bugs_reported()];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Pending);
    assert_eq!(
        decision.reasons.iter().map(|r| r.code).collect::<Vec<_>>(),
        [ReasonCode::BudgetDeferred, ReasonCode::RequiredRoleDegraded]
    );
    Ok(())
}
