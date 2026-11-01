//! The rubric's inputs for one pipeline run (04 §5 "Inputs", #60 ST-12): the facts Z4
//! authenticated, GitHub's state snapshot, and what the `admit` and `review` jobs reported,
//! with the decision key of 02 §5.
//!
//! Nothing here believes the LLM stage ran unless it can show it did (wi-b5affa). Until M2's
//! review exists the review job is a placeholder (D11), and evidence reuse is not built, so a
//! run admitted to spend or to reuse reads as an unavailable LLM, and only a stage that had no
//! reason to run (a draft while it is one, a tier without one) is excused.

use std::collections::BTreeSet;

use assay_domain::{
    DecisionInputs, DecisionKeyParts, Deferral, Fingerprint, InfraPolicy, LlmStage, Override,
    PullRequest, Readiness, SupportOutcome, Waiver, decision_key,
};
use serde::Serialize;
use thiserror::Error;

use super::AuthenticatedRun;
use super::request::{AdmitJob, AdmitOutcome, JobResult, ReviewJob, ReviewStage};
use crate::config::Sha256Digest;
use crate::eligibility::{Command, GitHubState, PriorWaivers, canonical_json};

/// What one decision is composed from, borrowed for the call.
#[derive(Debug, Clone, Copy)]
pub struct InputsRequest<'a> {
    /// The run Z4 authenticated.
    pub run: &'a AuthenticatedRun,
    /// GitHub's state snapshot of the run's head.
    pub state: &'a GitHubState,
    /// The PR's draft flag, read at decide time.
    pub readiness: Readiness,
    /// How many times this head's deferred run was requeued (the ledger's deferral record).
    pub requeues: u32,
    /// What the previous completed run waived, for the waivers carried to this head.
    pub prior: &'a PriorWaivers,
    /// The `admit` job's outputs.
    pub admit: AdmitJob,
    /// The `review` job's outputs.
    pub review: &'a ReviewJob,
}

/// Why no decision inputs could be composed. Nothing may be published from them.
#[derive(Debug, Error)]
pub enum InputsError {
    /// The snapshot is of another head than the run's: its approvals and commands are not this
    /// head's.
    #[error("the GitHub state snapshot is not of the run's head")]
    OtherHead,
    /// A part of the decision key could not be written as JSON: a defect in `decide`.
    #[error("could not encode the decision key's {part}")]
    Encode {
        /// The decision key field.
        part: &'static str,
        /// serde's error.
        #[source]
        source: serde_json::Error,
    },
}

/// The rubric's inputs for `request`'s run.
///
/// The LLM stage is what `admit` and `review` show, read the way rule 8 reads it
/// ([`LlmStage::effective`]): an `admit` that did not succeed, a reused admission and a spend
/// whose review did not run are `Deferred(InfraLlmUnavailable)`; a deferred admission keeps its
/// own code; a no-spend admission is `NotRun` with its reason, excused only while the snapshot
/// agrees (a draft still a draft, a tier with no LLM stage). The stage never completes here, so
/// there are no reviewer findings, dispositions, completed or degraded roles, and neither the
/// coordinator nor the verifier was due.
///
/// # Errors
///
/// [`InputsError::OtherHead`] when `state` is not a snapshot of the run's head;
/// [`InputsError::Encode`] when a part of the decision key cannot be encoded.
pub fn decision_inputs(request: &InputsRequest<'_>) -> Result<DecisionInputs, InputsError> {
    let InputsRequest {
        run,
        state,
        readiness,
        requeues,
        prior,
        admit,
        review,
    } = *request;
    if state.head() != &run.head {
        return Err(InputsError::OtherHead);
    }
    let pr = run.pull_request(state.snapshot(readiness, requeues));
    let llm = llm_stage(admit, review.stage).effective(&pr);
    let waivers = state.waivers_carried(prior, &run.waiver_bindings());
    let override_record = state.override_record();
    let evidence_key = run.run_key.to_string();
    let decision_key = compose_key(&KeyInputs {
        evidence_key: &evidence_key,
        on_infra_failure: run.policy.on_infra_failure,
        state,
        pr: &pr,
        llm,
        waivers: &waivers,
        override_record: override_record.as_ref(),
    })?;
    Ok(DecisionInputs {
        decision_key,
        evidence_key,
        // The inputs own their policy and gates; the run keeps its own for the bundle.
        policy: run.policy.clone(),
        pr,
        gates: run.gates.clone(),
        weigh_in: run.weigh_in,
        llm,
        findings: Vec::new(),
        dispositions: Vec::new(),
        completed_roles: BTreeSet::new(),
        coordinator: SupportOutcome::NotNeeded,
        verifier: SupportOutcome::NotNeeded,
        degraded: Vec::new(),
        waivers,
        override_record,
    })
}

/// What one decision's key (02 §5, D14) is composed from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeyInputs<'a> {
    /// The evidence key the decision is made over.
    pub(crate) evidence_key: &'a str,
    /// The policy's `on_infra_failure`.
    pub(crate) on_infra_failure: InfraPolicy,
    /// The snapshot whose approvals and dismissals the key covers.
    pub(crate) state: &'a GitHubState,
    /// The PR as the decision reads it: its draft flag and requeue count.
    pub(crate) pr: &'a PullRequest,
    /// The LLM stage as rule 8 reads it.
    pub(crate) llm: LlmStage,
    /// The waivers the decision applies.
    pub(crate) waivers: &'a [Waiver],
    /// The override it applies, if any.
    pub(crate) override_record: Option<&'a Override>,
}

/// The decision key of `parts`: the one composition of `decide`'s and decide-only's inputs.
///
/// # Errors
///
/// [`InputsError::Encode`] when a part cannot be encoded.
pub(crate) fn compose_key(parts: &KeyInputs<'_>) -> Result<String, InputsError> {
    let approvals = digest("approvals_digest", &approvals(parts.state))?;
    let waivers_digest = digest("waivers_digest", &parts.waivers)?;
    let override_state = canonical("override_state", &parts.override_record)?;
    let dismissals = digest("dismissals_digest", &dismissals(parts.state))?;
    let deferral_state = canonical("deferral_state", &DeferralState::of(parts.llm, parts.pr))?;
    Ok(decision_key(&DecisionKeyParts {
        evidence_key: parts.evidence_key,
        approvals_digest: approvals.as_str(),
        waivers_digest: waivers_digest.as_str(),
        override_state: &override_state,
        dismissals_digest: dismissals.as_str(),
        on_infra_failure: parts.on_infra_failure,
        deferral_state: &deferral_state,
    })
    .to_string())
}

/// The LLM stage as `admit` and `review` report it, before rule 8's reading.
const fn llm_stage(admit: AdmitJob, review: ReviewStage) -> LlmStage {
    const UNAVAILABLE: LlmStage = LlmStage::Deferred {
        reason: Deferral::InfraLlmUnavailable,
    };
    if !matches!(admit.result, JobResult::Success) {
        // An `admit` that failed could not read the ledger or take a slot (06 §1, F16).
        return UNAVAILABLE;
    }
    match admit.admission {
        AdmitOutcome::NoSpend(reason) => LlmStage::NotRun { reason },
        AdmitOutcome::Deferred(deferred) => LlmStage::Deferred {
            reason: deferred.deferral(),
        },
        // Rebuilding the stage from the reused bundle is not built (fail closed).
        AdmitOutcome::Reused => UNAVAILABLE,
        AdmitOutcome::Spend { .. } => match review {
            ReviewStage::NotRun => UNAVAILABLE,
        },
    }
}

/// `value`'s canonical JSON (compact, keys sorted), as one decision key `part`.
fn canonical<T: Serialize>(part: &'static str, value: &T) -> Result<String, InputsError> {
    canonical_json(value).map_err(|source| InputsError::Encode { part, source })
}

/// The sha256 of `value`'s canonical JSON, as one decision key `part`.
fn digest<T: Serialize>(part: &'static str, value: &T) -> Result<Sha256Digest, InputsError> {
    canonical(part, value).map(|json| Sha256Digest::of(json.as_bytes()))
}

/// One reviewer's standing approval of the head, in the decision key.
#[derive(Serialize)]
struct ApprovalPart {
    review: u64,
    reviewer: u64,
    counted: bool,
}

/// The head's standing approvals, by reviewer id.
fn approvals(state: &GitHubState) -> Vec<ApprovalPart> {
    state
        .approval_facts()
        .map(|(review, reviewer, counted)| ApprovalPart {
            review: review.get(),
            reviewer: reviewer.0.get(),
            counted,
        })
        .collect()
}

/// One valid dismissal of the PR, in the decision key.
#[derive(Serialize)]
struct DismissalPart<'a> {
    comment_id: u64,
    actor_id: u64,
    fp: Fingerprint,
    reason: &'a str,
}

/// The PR's valid dismissals, in record order.
fn dismissals(state: &GitHubState) -> Vec<DismissalPart<'_>> {
    state
        .dismissals()
        .filter_map(|record| match &record.command {
            Command::Dismiss { finding, reason } => Some(DismissalPart {
                comment_id: record.comment_id.0.get(),
                actor_id: record.actor.0.get(),
                fp: *finding,
                reason,
            }),
            _ => None,
        })
        .collect()
}

/// What the LLM stage's deferral rule (8a) and its draft excuse read, in the decision key: the
/// stage as rule 8 reads it, the draft flag and the requeue count.
#[derive(Serialize)]
struct DeferralState {
    llm: LlmStage,
    readiness: Readiness,
    requeues: u32,
}

impl DeferralState {
    const fn of(llm: LlmStage, pr: &PullRequest) -> Self {
        Self {
            llm,
            readiness: pr.readiness,
            requeues: pr.requeues,
        }
    }
}
