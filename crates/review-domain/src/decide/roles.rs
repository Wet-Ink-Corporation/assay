//! Which roles of the LLM stage ran degraded (`docs/architecture/04-pipeline.md` §3 "Required
//! roles", §5 rules 8b and 8c): nothing downgrades silently.

use std::collections::{BTreeMap, BTreeSet};

use crate::decide::inputs::{DecisionInputs, LlmStage, StageRole, SupportOutcome};
use crate::decision::DegradedRole;
use crate::severity::Severity;
use crate::tier::Tier;
use crate::tiering::required_roles;

/// The reason a role is listed with when it was due but never reported.
const MISSING: &str = "missing";

/// What joins the reasons of one role reported degraded more than once (`cap, timeout`).
const REASON_SEPARATOR: &str = ", ";

/// Every degraded role, sorted by role and listed once: those reported degraded, a failed
/// coordinator or verifier, and, when the stage completed, every call that was due but never
/// reported (see [`missing`]).
///
/// A role degraded for several reasons (two shards, or a reported and a failed outcome) keeps
/// all of them, distinct, sorted and joined with `", "`, so no cause is dropped from the summary.
pub(super) fn degraded_roles(inputs: &DecisionInputs) -> Vec<DegradedRole> {
    // Keyed by wire code, so the roles come out sorted as the decision lists them.
    let mut reasons: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut add = |role: StageRole, reason| {
        reasons.entry(role.code()).or_default().insert(reason);
    };
    for reported in &inputs.degraded {
        add(reported.role, reported.reason.code());
    }
    for (role, outcome) in [
        (StageRole::Coordinator, &inputs.coordinator),
        (StageRole::Verifier, &inputs.verifier),
    ] {
        if let SupportOutcome::Failed { reason } = outcome {
            add(role, reason.code());
        }
    }
    if matches!(inputs.llm.effective(&inputs.pr), LlmStage::Completed) {
        for role in missing(inputs) {
            add(role, MISSING);
        }
    }
    reasons
        .into_iter()
        .map(|(role, why)| DegradedRole {
            role: role.to_owned(),
            reason: why.into_iter().collect::<Vec<_>>().join(REASON_SEPARATOR),
        })
        .collect()
}

/// The calls a completed stage owed but that never reported (04 §3). A call reported in
/// `degraded` did report, so it is never also `missing`:
/// - each role the PR's tier and classes require that neither completed nor reported degraded;
/// - on `full`, a coordinator that did not run although there are reviewer findings (wi-98c82b);
/// - on `full` or `lite`, a verifier that did not run although a blocker or major, a candidate
///   of 04 §3, was raised (`micro` has no verifier).
fn missing(inputs: &DecisionInputs) -> Vec<StageRole> {
    let pr = &inputs.pr;
    let reported = |role: StageRole| inputs.degraded.iter().any(|d| d.role == role);
    let mut missing: Vec<StageRole> = required_roles(pr.tier, !pr.classes.is_empty())
        .into_iter()
        .filter(|role| !inputs.completed_roles.contains(role))
        .map(StageRole::from)
        .filter(|&role| !reported(role))
        .collect();
    let ungrouped = pr.tier == Tier::Full
        && inputs.coordinator == SupportOutcome::NotNeeded
        && !inputs.findings.is_empty()
        && !reported(StageRole::Coordinator);
    if ungrouped {
        missing.push(StageRole::Coordinator);
    }
    let unverified = matches!(pr.tier, Tier::Full | Tier::Lite)
        && inputs.verifier == SupportOutcome::NotNeeded
        && inputs
            .findings
            .iter()
            .any(|f| f.severity >= Severity::Major)
        && !reported(StageRole::Verifier);
    if unverified {
        missing.push(StageRole::Verifier);
    }
    missing
}
