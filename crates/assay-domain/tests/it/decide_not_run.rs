//! An LLM stage that did not run is excused only by a tier without one or a draft; anywhere
//! else it is an unavailable LLM, so nothing downgrades silently (04 §3, rule 8).

use std::error::Error;

use assay_domain::{
    Conclusion, Decision, DecisionInputs, LlmStage, NotRunReason, OwnerApproval, PrClass,
    ReasonCode, Tier, decide,
};

use crate::support::{clean_inputs, reason, rubric_v1};

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

fn not_run(tier: Tier, reason: NotRunReason) -> DecisionInputs {
    let mut inputs = clean_inputs();
    inputs.pr.tier = tier;
    inputs.llm = LlmStage::NotRun { reason };
    inputs
}

#[test]
fn a_trivial_pr_without_an_llm_stage_succeeds() -> Result<(), Box<dyn Error>> {
    let decision = decided(&not_run(Tier::Trivial, NotRunReason::NoLlmTier))?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn a_draft_without_an_llm_stage_succeeds() -> Result<(), Box<dyn Error>> {
    let decision = decided(&not_run(Tier::Full, NotRunReason::Draft))?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn a_reviewed_tier_without_an_llm_stage_warns_llm_unavailable() -> Result<(), Box<dyn Error>> {
    let decision = decided(&not_run(Tier::Lite, NotRunReason::NoLlmTier))?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "infra-llm-unavailable")]
    );
    Ok(())
}

#[test]
fn a_trust_path_pr_without_its_llm_stage_is_pending() -> Result<(), Box<dyn Error>> {
    let mut inputs = not_run(Tier::Full, NotRunReason::NoLlmTier);
    inputs.pr.classes = vec![PrClass::TrustPath];
    inputs.pr.owner_approval = OwnerApproval::Approved;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Pending);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::InfraLlmUnavailable,
            "requeued 0 of 3 times"
        )]
    );
    Ok(())
}
