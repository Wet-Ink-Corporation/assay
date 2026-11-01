//! decide-only's decision inputs (D15, QA-08): the evidence bundle's inputs with the facts a
//! fresh GitHub state snapshot changes recomputed, and the decision key composed by the one
//! rule `decide` uses.

use assay_domain::DecisionInputs;

use super::EvidenceBundle;
use crate::decide::{InputsError, KeyInputs, compose_key};
use crate::eligibility::GitHubState;

/// What one re-decision is composed from, borrowed for the call.
#[derive(Debug, Clone, Copy)]
pub struct RedecisionRequest<'a> {
    /// The authenticated evidence bundle of the head.
    pub evidence: &'a EvidenceBundle,
    /// decide-only's snapshot of that head ([`redecided_state`](crate::redecided_state)).
    pub state: &'a GitHubState,
    /// How many times this head's deferred run was requeued (the ledger's deferral record).
    pub requeues: u32,
}

/// The rubric's inputs for re-deciding the evidence bundle's head over `request.state`.
///
/// What a snapshot decides is recomputed: owner approval and authorship from `request.state`,
/// the requeue count from `request.requeues`, the head's waivers with those the evidence
/// decision waived carried through its own bindings while a waive record of them still stands,
/// the override, and the decision key (with the dismissals and approvals it covers). Everything
/// else is the bundle's: the evidence key, policy, gates, Weigh-In, LLM stage, findings,
/// dispositions, roles, support outcomes, the tier and classes, and the draft flag (D15: a
/// ready-for-review flip runs the pipeline again).
///
/// # Errors
///
/// [`InputsError::OtherHead`] when `request.state` is not a snapshot of the bundle's head;
/// [`InputsError::Encode`] when a part of the decision key cannot be encoded.
pub fn redecision_inputs(request: &RedecisionRequest<'_>) -> Result<DecisionInputs, InputsError> {
    let RedecisionRequest {
        evidence,
        state,
        requeues,
    } = *request;
    if state.head() != evidence.head() {
        return Err(InputsError::OtherHead);
    }
    let recorded = evidence.inputs();
    // The new inputs own their PR; the bundle keeps the recorded one.
    let mut pr = recorded.pr.clone();
    pr.owner_approval = state.owner_approval();
    pr.authorship = state.authorship();
    pr.requeues = requeues;
    let waivers = state.waivers_carried(evidence.prior_waivers(), evidence.bindings());
    let override_record = state.override_record();
    let decision_key = compose_key(&KeyInputs {
        evidence_key: &recorded.evidence_key,
        on_infra_failure: recorded.policy.on_infra_failure,
        state,
        pr: &pr,
        llm: recorded.llm,
        waivers: &waivers,
        override_record: override_record.as_ref(),
    })?;
    // Each field below is copied from the bundle into inputs the caller owns.
    Ok(DecisionInputs {
        decision_key,
        evidence_key: recorded.evidence_key.clone(),
        policy: recorded.policy.clone(),
        pr,
        gates: recorded.gates.clone(),
        weigh_in: recorded.weigh_in,
        llm: recorded.llm,
        findings: recorded.findings.clone(),
        dispositions: recorded.dispositions.clone(),
        completed_roles: recorded.completed_roles.clone(),
        coordinator: recorded.coordinator,
        verifier: recorded.verifier,
        degraded: recorded.degraded.clone(),
        waivers,
        override_record,
    })
}
