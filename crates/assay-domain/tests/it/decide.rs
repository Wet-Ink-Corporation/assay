//! `decide` rules 1 to 5 and 9 of `docs/architecture/04-pipeline.md` §5, and the precedence
//! between rules.

use std::error::Error;

use assay_domain::{
    Conclusion, Decision, DecisionInputs, GateConclusion, GateOutcome, LlmStage, NotRunReason,
    OwnerApproval, PrClass, ReasonCode, Tier, WeighIn, decide,
};

use crate::support::{clean_inputs, gate, reason, rubric_v1};

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

#[test]
fn clean_inputs_succeed_with_no_reasons() -> Result<(), Box<dyn Error>> {
    let decision = decided(&clean_inputs())?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn the_decision_carries_the_keys_and_the_rubric_version() -> Result<(), Box<dyn Error>> {
    let decision = decided(&clean_inputs())?;

    assert_eq!(decision.key, "dec-42-1");
    assert_eq!(decision.evidence_key, "run-42-1");
    assert_eq!(decision.rubric_version, "1");
    Ok(())
}

#[test]
fn a_failed_required_gate_fails_the_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![gate("temper", GateConclusion::Fail, Vec::new())];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    Ok(())
}

#[test]
fn a_gate_error_fails_the_pr_with_its_detail() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    let mut errored = gate("temper", GateConclusion::Pass, Vec::new());
    errored.outcome = GateOutcome::Error {
        detail: "timeout".to_owned(),
    };
    inputs.gates = vec![errored];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::GateError, "temper: timeout")]
    );
    Ok(())
}

#[test]
fn a_skipped_required_gate_fails_the_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    let mut skipped = gate("temper", GateConclusion::Pass, Vec::new());
    skipped.outcome = GateOutcome::Skipped;
    inputs.gates = vec![skipped];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::GateSkipped, "temper")]
    );
    Ok(())
}

/// Required means pass (#161, owner 2026-11-01): a required gate that concluded `neutral` fails
/// rule 1 with its own reason.
#[test]
fn a_neutral_required_gate_fails_the_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![gate("custom-lint", GateConclusion::Neutral, Vec::new())];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::GateNeutral, "custom-lint")]
    );
    Ok(())
}

#[test]
fn a_neutral_optional_gate_does_not_fail_the_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    let mut optional = gate("custom-lint", GateConclusion::Neutral, Vec::new());
    optional.required = false;
    inputs.gates.push(optional);

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn a_failed_optional_gate_does_not_fail_the_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    let mut optional = gate("custom-lint", GateConclusion::Fail, Vec::new());
    optional.required = false;
    inputs.gates.push(optional);

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn an_unprotected_base_fails_base_not_protected() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.base_ref = "release".to_owned();

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::BaseNotProtected, "release")]
    );
    Ok(())
}

#[test]
fn two_open_prs_on_the_head_fail_ambiguous_head() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.open_prs_on_head = 2;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::AmbiguousHead, "2 open PRs on this head")]
    );
    Ok(())
}

#[test]
fn a_needs_split_tier_fails_needs_split() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::NeedsSplit;
    inputs.llm = LlmStage::NotRun {
        reason: NotRunReason::NoLlmTier,
    };

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::NeedsSplit, "over the line budget")]
    );
    Ok(())
}

#[test]
fn an_unapproved_trust_path_change_fails_trust_path_unapproved() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.pr.classes = vec![PrClass::TrustPath, PrClass::ExecSurface];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::TrustPathUnapproved,
            "trust-path, exec-surface"
        )]
    );
    Ok(())
}

#[test]
fn an_owner_approved_trust_path_change_succeeds() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.pr.classes = vec![PrClass::ExecSurface];
    inputs.pr.owner_approval = OwnerApproval::Approved;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn a_required_weigh_in_gate_fails_on_each_blocking_state() -> Result<(), Box<dyn Error>> {
    let states = [
        (WeighIn::OpenBlocking, ReasonCode::WeighInOpenBlocking),
        (WeighIn::PendingTriage, ReasonCode::WeighInPendingTriage),
        (WeighIn::NoEvidence, ReasonCode::WeighInNoEvidence),
    ];

    for (state, code) in states {
        let mut inputs = clean_inputs();
        inputs.weigh_in = state;

        let decision = decided(&inputs)?;

        assert_eq!(decision.conclusion, Conclusion::Failure, "{state:?}");
        assert_eq!(decision.reasons, [reason(code, "weigh-in")], "{state:?}");
    }
    Ok(())
}

#[test]
fn a_clear_weigh_in_gate_succeeds() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.weigh_in = WeighIn::Clear;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    Ok(())
}

#[test]
fn every_firing_rule_is_a_reason_in_rule_order() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.weigh_in = WeighIn::OpenBlocking;
    inputs.pr.classes = vec![PrClass::TrustPath];
    inputs.pr.base_ref = "release".to_owned();
    inputs.gates = vec![gate("temper", GateConclusion::Fail, Vec::new())];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [
            reason(ReasonCode::GateFailed, "temper"),
            reason(ReasonCode::BaseNotProtected, "release"),
            reason(ReasonCode::TrustPathUnapproved, "trust-path"),
            reason(ReasonCode::WeighInOpenBlocking, "weigh-in"),
        ]
    );
    Ok(())
}

#[test]
fn a_gate_failure_wins_over_a_deferred_llm_stage() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.classes = vec![PrClass::TrustPath];
    inputs.pr.owner_approval = OwnerApproval::Approved;
    inputs.llm = LlmStage::Deferred {
        reason: assay_domain::Deferral::BudgetDeferred,
    };
    inputs.gates = vec![gate("temper", GateConclusion::Fail, Vec::new())];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [
            reason(ReasonCode::GateFailed, "temper"),
            reason(ReasonCode::BudgetDeferred, "requeued 0 of 3 times"),
        ]
    );
    Ok(())
}
