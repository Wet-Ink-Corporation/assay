//! `plan_stage` (#60 ST-11): what `admit` makes of the plan before it reads any port, so the
//! `assay admit` job reads nothing for a run with no LLM stage by the same rule `admit` applies.

use std::assert_matches;
use std::error::Error;

use assay_app::{Admission, ContractValidator, PlanDocument, PlanStage, plan_stage};
use assay_domain::NotRunReason;

use crate::admit_fakes::{plan, this_run};

#[test]
fn a_draft_passes_its_plan_through_unchanged() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let draft = plan("lite").draft().bytes();

    let stage = plan_stage(&validator, &draft, this_run(1))?;

    let PlanStage::NoSpend(admitted) = stage else {
        return Err(format!("a draft spends: {stage:?}").into());
    };
    assert_eq!(
        admitted.admission(),
        &Admission::NoSpend(NotRunReason::Draft)
    );
    assert_eq!(admitted.run_json(), draft);
    Ok(())
}

#[test]
fn a_tier_without_an_llm_stage_does_not_spend() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    for tier in ["trivial", "needs-split"] {
        let stage = plan_stage(&validator, &plan(tier).bytes(), this_run(1))?;

        assert_matches!(
            stage,
            PlanStage::NoSpend(ref admitted)
                if admitted.admission() == &Admission::NoSpend(NotRunReason::NoLlmTier),
            "{tier}"
        );
    }
    Ok(())
}

#[test]
fn a_ready_reviewed_plan_spends() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    for tier in ["full", "lite", "micro"] {
        let stage = plan_stage(&validator, &plan(tier).bytes(), this_run(1))?;

        assert_matches!(stage, PlanStage::Spends, "{tier}");
    }
    Ok(())
}

#[test]
fn a_plan_of_another_run_is_refused() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let mut other = this_run(1);
    other.run_id = other.run_id.saturating_add(1);

    let refused = plan_stage(&validator, &plan("lite").bytes(), other);

    assert_matches!(refused, Err(PlanDocument::OtherRun { .. }));
    Ok(())
}
