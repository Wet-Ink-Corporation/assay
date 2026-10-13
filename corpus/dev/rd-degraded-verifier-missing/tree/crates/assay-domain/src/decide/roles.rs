//! Which roles of the LLM stage ran degraded (`docs/architecture/04-pipeline.md` §3 "Required
//! roles", §5 rules 8b and 8c): nothing downgrades silently.

use crate::decide::inputs::{DecisionInputs, LlmStage, SupportOutcome};
use crate::decision::DegradedRole;
use crate::tier::Tier;
use crate::tiering::required_roles;

/// The reason a required role or the coordinator is listed with when it never reported.
const MISSING: &str = "missing";

/// Every degraded role, sorted and without duplicates: those reported degraded, a failed
/// coordinator or verifier, and, when the stage completed, each required role that did not
/// complete and a `full` coordinator that did not run although there are findings.
///
/// The roles and reasons are copied: the decision is a new document that owns them.
pub(super) fn degraded_roles(inputs: &DecisionInputs) -> Vec<DegradedRole> {
    let mut degraded = inputs.degraded.clone();
    degraded.extend(failed("coordinator", &inputs.coordinator));
    degraded.extend(failed("verifier", &inputs.verifier));
    if matches!(inputs.llm.effective(&inputs.pr), LlmStage::Completed) {
        degraded.extend(missing(inputs));
    }
    degraded.sort_unstable();
    degraded.dedup();
    degraded
}

/// A support call that failed, as a degraded role.
fn failed(role: &str, outcome: &SupportOutcome) -> Option<DegradedRole> {
    match outcome {
        SupportOutcome::Failed { reason } => Some(DegradedRole {
            role: role.to_owned(),
            reason: reason.clone(),
        }),
        SupportOutcome::NotNeeded | SupportOutcome::Completed => None,
    }
}

/// The required roles that neither completed nor were reported degraded, and a `full`
/// coordinator that was not run over reviewer findings.
fn missing(inputs: &DecisionInputs) -> Vec<DegradedRole> {
    let pr = &inputs.pr;
    let reported = |code: &str| inputs.degraded.iter().any(|d| d.role == code);
    let mut missing: Vec<DegradedRole> = required_roles(pr.tier, !pr.classes.is_empty())
        .into_iter()
        .filter(|role| !inputs.completed_roles.contains(role) && !reported(role.code()))
        .map(|role| absent(role.code()))
        .collect();
    let ungrouped = pr.tier == Tier::Full
        && inputs.coordinator == SupportOutcome::NotNeeded
        && !inputs.findings.is_empty();
    if ungrouped {
        missing.push(absent("coordinator"));
    }
    missing
}

fn absent(role: &str) -> DegradedRole {
    DegradedRole {
        role: role.to_owned(),
        reason: MISSING.to_owned(),
    }
}
