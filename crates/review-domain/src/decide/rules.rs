//! The rules of rubric v1 (`docs/architecture/04-pipeline.md` §5), each a pure check that
//! yields the reasons it fires with.

use std::collections::BTreeSet;

use crate::decide::inputs::{
    Authorship, DecisionInputs, Deferral, GateOutcome, GateReport, InfraPolicy, LlmPolicy,
    LlmStage, OwnerApproval, ReviewedFinding, WeighIn,
};
use crate::decide::waive::Waivers;
use crate::decision::{DegradedRole, Reason};
use crate::event::VerifierVerdict;
use crate::findings::GateConclusion;
use crate::reason::ReasonCode;
use crate::rubric::Rubric;
use crate::severity::Severity;
use crate::tier::Tier;

/// What a firing rule does to the conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Effect {
    /// Failure from rules 1 to 7, which only an `all` override neutralises.
    Fail,
    /// Failure from rule 8 (8a after its requeues, 8b).
    LlmFail,
    /// Pending: rule 8a.
    LlmPending,
    /// Success with a visible warning: rule 8c.
    LlmWarn,
}

/// One reason a rule fired with, and its effect.
#[derive(Debug)]
pub(super) struct Firing {
    pub(super) effect: Effect,
    pub(super) reason: Reason,
}

fn firing(effect: Effect, code: ReasonCode, detail: impl Into<String>) -> Firing {
    Firing {
        effect,
        reason: Reason {
            code,
            detail: detail.into(),
            fp: None,
        },
    }
}

/// Every reason of every rule that fires, in rule order.
pub(super) fn evaluate(
    rubric: &Rubric,
    inputs: &DecisionInputs,
    waivers: &Waivers<'_>,
    degraded: &[DegradedRole],
) -> Vec<Firing> {
    let mut firings = Vec::new();
    firings.extend(gates(inputs, waivers));
    firings.extend(pull_request(inputs));
    firings.extend(weigh_in(inputs.weigh_in));
    firings.extend(blockers(inputs));
    firings.extend(majors(rubric, inputs));
    firings.extend(llm(rubric, inputs, degraded));
    firings
}

/// Rule 1: a required gate failed (minus waived findings), concluded `neutral`, errored or was
/// skipped.
fn gates<'a>(
    inputs: &'a DecisionInputs,
    waivers: &'a Waivers<'_>,
) -> impl Iterator<Item = Firing> + 'a {
    inputs
        .gates
        .iter()
        .filter(|gate| gate.required)
        .filter_map(|gate| match &gate.outcome {
            GateOutcome::Ran {
                conclusion: GateConclusion::Fail,
                steps,
            } if !steps.all_failures_are_findings() || !fully_waived(gate, waivers) => {
                Some(firing(Effect::Fail, ReasonCode::GateFailed, &*gate.name))
            }
            // Required means pass (#161): a neutral conclusion is not attributed to findings, so
            // no waiver clears it.
            GateOutcome::Ran {
                conclusion: GateConclusion::Neutral,
                ..
            } => Some(firing(Effect::Fail, ReasonCode::GateNeutral, &*gate.name)),
            GateOutcome::Ran { .. } => None,
            GateOutcome::Error { detail } => Some(firing(
                Effect::Fail,
                ReasonCode::GateError,
                format!("{}: {detail}", gate.name),
            )),
            GateOutcome::Skipped => {
                Some(firing(Effect::Fail, ReasonCode::GateSkipped, &*gate.name))
            }
        })
}

/// A failed gate whose every finding was waived no longer fails; one with no findings cannot be
/// waived.
fn fully_waived(gate: &GateReport, waivers: &Waivers<'_>) -> bool {
    !gate.findings.is_empty() && gate.findings.iter().all(|finding| waivers.covers(finding))
}

/// Rules 2, 2a, 3 and 4: the base, the head, the tier and owner approval.
fn pull_request(inputs: &DecisionInputs) -> Vec<Firing> {
    let pr = &inputs.pr;
    let mut firings = Vec::new();
    if !inputs.policy.protected_bases.contains(&pr.base_ref) {
        firings.push(firing(
            Effect::Fail,
            ReasonCode::BaseNotProtected,
            &*pr.base_ref,
        ));
    }
    if pr.open_prs_on_head > 1 {
        firings.push(firing(
            Effect::Fail,
            ReasonCode::AmbiguousHead,
            format!("{} open PRs on this head", pr.open_prs_on_head),
        ));
    }
    if pr.tier == Tier::NeedsSplit {
        firings.push(firing(
            Effect::Fail,
            ReasonCode::NeedsSplit,
            "over the line budget",
        ));
    }
    if !pr.classes.is_empty() && pr.owner_approval == OwnerApproval::NotApproved {
        let classes: Vec<&str> = pr.classes.iter().map(|class| class.code()).collect();
        firings.push(firing(
            Effect::Fail,
            ReasonCode::TrustPathUnapproved,
            classes.join(", "),
        ));
    }
    firings
}

/// Rule 5: a required Weigh-In gate reports a blocking state.
fn weigh_in(state: WeighIn) -> Option<Firing> {
    let code = match state {
        WeighIn::NotRequired | WeighIn::Clear => return None,
        WeighIn::OpenBlocking => ReasonCode::WeighInOpenBlocking,
        WeighIn::PendingTriage => ReasonCode::WeighInPendingTriage,
        WeighIn::NoEvidence => ReasonCode::WeighInNoEvidence,
    };
    Some(firing(Effect::Fail, code, "weigh-in"))
}

/// A finding the verifier confirmed about code the diff introduced.
fn confirmed_introduced(finding: &ReviewedFinding) -> bool {
    finding.verdict == Some(VerifierVerdict::Confirmed) && !finding.pre_existing
}

/// Confirmed, introduced blockers (rule 6, and `advisory_blockers`).
pub(super) fn verified_blockers(inputs: &DecisionInputs) -> impl Iterator<Item = &ReviewedFinding> {
    inputs
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Blocker && confirmed_introduced(f))
}

/// Rule 6: a verified blocker, while `llm_blockers` blocks.
fn blockers(inputs: &DecisionInputs) -> Vec<Firing> {
    if inputs.policy.llm_blockers != LlmPolicy::Block {
        return Vec::new();
    }
    verified_blockers(inputs)
        .map(|finding| Firing {
            effect: Effect::Fail,
            reason: Reason {
                code: ReasonCode::BlockerVerified,
                detail: "verified blocker".to_owned(),
                fp: Some(finding.fp),
            },
        })
        .collect()
}

/// Rule 7: enough distinct root causes of confirmed, introduced majors from non-micro roles,
/// while `llm_majors` blocks.
fn majors(rubric: &Rubric, inputs: &DecisionInputs) -> Option<Firing> {
    if inputs.policy.llm_majors != LlmPolicy::Block {
        return None;
    }
    let groups: BTreeSet<&str> = inputs
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Major && confirmed_introduced(f))
        .filter(|f| !rubric.is_micro_role(&f.role))
        .map(|f| f.group_id.as_str())
        .collect();
    let threshold = rubric.majors_block_threshold().get();
    // More groups than u32::MAX still exceed any threshold.
    let count = u32::try_from(groups.len()).unwrap_or(u32::MAX);
    (count >= threshold).then(|| {
        firing(
            Effect::Fail,
            ReasonCode::MajorsOverThreshold,
            format!("{count} confirmed majors by root cause (threshold {threshold})"),
        )
    })
}

/// Rule 8: a deferred LLM stage (8a) or `degraded` roles (8b), each a warning otherwise (8c).
fn llm(rubric: &Rubric, inputs: &DecisionInputs, degraded: &[DegradedRole]) -> Vec<Firing> {
    let pr = &inputs.pr;
    let policy = &inputs.policy;
    let blocks_on_infra = policy.on_infra_failure == InfraPolicy::Block
        || pr
            .classes
            .iter()
            .any(|class| policy.block_on_infra_for.contains(class));
    let mut firings = Vec::new();
    if let LlmStage::Deferred { reason } = inputs.llm.effective(pr) {
        let holds =
            blocks_on_infra || (reason == Deferral::SlotBusy && pr.authorship == Authorship::Agent);
        let max = rubric.max_requeues();
        firings.push(if !holds {
            firing(Effect::LlmWarn, ReasonCode::LlmDegraded, reason.code())
        } else if pr.requeues >= max {
            firing(
                Effect::LlmFail,
                ReasonCode::InfraLlmUnavailable,
                format!("{} after {max} requeues", reason.code()),
            )
        } else {
            firing(
                Effect::LlmPending,
                reason.reason_code(),
                format!("requeued {} of {max} times", pr.requeues),
            )
        });
    }
    firings.extend(degraded.iter().map(|role| {
        let detail = format!("{}: {}", role.role, role.reason);
        if blocks_on_infra {
            firing(Effect::LlmFail, ReasonCode::RequiredRoleDegraded, detail)
        } else {
            firing(Effect::LlmWarn, ReasonCode::LlmDegraded, detail)
        }
    }));
    firings
}
