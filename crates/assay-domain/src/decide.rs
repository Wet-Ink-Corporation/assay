//! `decide`: the pure function from validated inputs and a rubric to a decision
//! (`docs/architecture/04-pipeline.md` §5, P1, ADR-04).

pub(crate) mod digest;
pub(crate) mod inputs;
mod roles;
mod rules;
mod waive;

use std::collections::BTreeSet;

use crate::decision::{Conclusion, Counts, Decision, DecisionSchema, FindingDisposition, Reason};
use crate::disposition::Disposition;
use crate::fingerprint::Fingerprint;
use crate::reason::ReasonCode;
use crate::rubric::Rubric;
use crate::severity::Severity;
use inputs::{DecisionInputs, LlmPolicy, OverrideScope};
use rules::{Effect, Firing};
use waive::Waivers;
pub use waive::{WaiverRefusal, waivable_fingerprints, waiver_refusal};

/// Decides one PR head: rubric v1 rules 1 to 9, waive and override of 04 §5.
///
/// The conclusion is that of the first rule that fires, in the order 1, 2, 2a, 3, 4, 5, 6, 7,
/// 8a, 8b, 8c (rule 9 is success); `pending` comes only from rule 8a. The reasons are those of
/// every rule that fires, in that order, and are kept when an override turns the conclusion
/// `neutral`. A waiver removes one human-decision gate finding the rubric lists from rule 1; an
/// override is void unless the reasons hash to the digest it quotes, never applies when rule 2
/// fired (`base-not-protected` is always `failure`, R3-03), and scope `llm` only neutralises a
/// decision whose every failing or pending reason comes from rule 8. A required
/// role of the PR's tier and classes that did not complete, a coordinator or verifier that
/// failed or was due but did not run, and every role reported degraded are listed once each in
/// `degraded` and fire rule 8b or 8c.
///
/// Pure and total: the same rubric and inputs give the same decision, whose `inputs_digest`
/// identifies both. The decision is a new document that owns its data: the keys, fingerprints
/// and dispositions it repeats from the borrowed inputs are copied into it, and its degraded
/// roles are derived from them.
#[must_use]
pub fn decide(rubric: &Rubric, inputs: &DecisionInputs) -> Decision {
    let waivers = Waivers::resolve(rubric, inputs);
    let waived = waivers.waived();
    let degraded = roles::degraded_roles(inputs);
    let firings = rules::evaluate(rubric, inputs, &waivers, &degraded);

    let mut conclusion = conclusion_of(&firings);
    let overridden = overriding_scope(inputs, conclusion, &firings);
    let mut reasons: Vec<Reason> = firings.into_iter().map(|firing| firing.reason).collect();
    if let Some(scope) = overridden {
        conclusion = Conclusion::Neutral;
        reasons.push(Reason {
            code: ReasonCode::Override,
            detail: scope.code().to_owned(),
            fp: None,
        });
    }

    let mut dispositions: Vec<FindingDisposition> = inputs
        .dispositions
        .iter()
        .cloned()
        .chain(waived.iter().map(|w| FindingDisposition {
            fp: w.fp,
            code: Disposition::WaivedByOwner,
        }))
        .collect();
    dispositions.sort_unstable();
    dispositions.dedup();

    Decision {
        schema: DecisionSchema::V1,
        key: inputs.decision_key.clone(),
        evidence_key: inputs.evidence_key.clone(),
        conclusion,
        reasons,
        counts: counts(inputs, &waivers),
        advisory_blockers: advisory_blockers(inputs),
        dispositions,
        waived,
        degraded,
        rubric_version: rubric.version().to_owned(),
        inputs_digest: digest::inputs_digest(rubric, inputs),
    }
}

/// The conclusion of the first rule that fires with a failing or pending effect.
fn conclusion_of(firings: &[Firing]) -> Conclusion {
    firings
        .iter()
        .find_map(|firing| match firing.effect {
            Effect::Fail | Effect::LlmFail => Some(Conclusion::Failure),
            Effect::LlmPending => Some(Conclusion::Pending),
            Effect::LlmWarn => None,
        })
        .unwrap_or(Conclusion::Success)
}

/// The scope of the inputs' override, if it is valid and applies to this conclusion.
fn overriding_scope(
    inputs: &DecisionInputs,
    conclusion: Conclusion,
    firings: &[Firing],
) -> Option<OverrideScope> {
    let record = inputs.override_record.as_ref()?;
    if record.by.is_empty() {
        return None;
    }
    if !matches!(conclusion, Conclusion::Failure | Conclusion::Pending) {
        return None;
    }
    if record.reasons_digest != digest::reasons_digest(firings.iter().map(|f| &f.reason)) {
        return None;
    }
    // The check is not required on an unprotected base: `neutral` there would only let a stale
    // check pass after a retarget, so rule 2 is never overridden (R3-03).
    if firings
        .iter()
        .any(|f| f.reason.code == ReasonCode::BaseNotProtected)
    {
        return None;
    }
    let outside_rule_8 = firings.iter().any(|f| f.effect == Effect::Fail);
    match record.scope {
        OverrideScope::Llm if outside_rule_8 => None,
        scope => Some(scope),
    }
}

/// Reviewer findings plus gate findings not waived, by severity.
fn counts(inputs: &DecisionInputs, waivers: &Waivers<'_>) -> Counts {
    let reviewer = inputs.findings.iter().map(|f| f.severity);
    let gates = inputs
        .gates
        .iter()
        .flat_map(|gate| &gate.findings)
        .filter(|f| !waivers.covers(f))
        .map(|f| f.severity);
    reviewer
        .chain(gates)
        .fold(Counts::default(), |mut counts, severity| {
            let slot = match severity {
                Severity::Blocker => &mut counts.blocker,
                Severity::Major => &mut counts.major,
                Severity::Minor => &mut counts.minor,
                Severity::Nit => &mut counts.nit,
            };
            *slot = slot.saturating_add(1);
            counts
        })
}

/// Verified blockers published, not failing, while `llm_blockers` is advisory.
fn advisory_blockers(inputs: &DecisionInputs) -> Vec<Fingerprint> {
    if inputs.policy.llm_blockers != LlmPolicy::Advisory {
        return Vec::new();
    }
    let fps: BTreeSet<Fingerprint> = rules::verified_blockers(inputs).map(|f| f.fp).collect();
    fps.into_iter().collect()
}
