//! The rubric's inputs for a local check: the facts `plan` recomputed, the local gates and
//! review, and fixed facts for what a working tree has no PR for.
//!
//! A working tree has no approval, author, draft flag or requeue, and no waivers or override:
//! it is a ready PR by someone other than an agent, never approved and never requeued, the only
//! open PR on its head. No coordinator or verifier runs locally, so where one would be due the
//! decision reports it failed rather than pretending it was not needed.

use std::collections::BTreeSet;

use assay_domain::{
    Authorship, CallFailure, DecisionInputs, Deferral, DegradedReport, GateReport, LlmStage,
    NotRunReason, OwnerApproval, Policy, PullRequest, Readiness, RequiredRole, ReviewedFinding,
    Severity, StageRole, SupportOutcome, Tier, Tiering, WeighIn,
};

use super::review::{LocalRole, Reviewed, Stage};
use crate::config::{Adapter, PluginsConfig};
use crate::plan::EvidenceKey;

/// What the decision reads besides the gates and the review.
#[derive(Debug)]
pub(super) struct Facts<'a> {
    /// The tree fingerprint, for the decision key.
    pub(super) tree: &'a str,
    pub(super) run_key: EvidenceKey,
    pub(super) policy: Policy,
    pub(super) base_ref: &'a str,
    pub(super) tiering: &'a Tiering,
    pub(super) plugins: &'a PluginsConfig,
}

impl Facts<'_> {
    /// The decision inputs of `gates` and the review, `None` when the tier has no LLM stage.
    pub(super) fn inputs(
        self,
        gates: Vec<GateReport>,
        reviewed: Option<&(LocalRole, Reviewed)>,
    ) -> DecisionInputs {
        let tier = self.tiering.tier;
        let review = Review::of(reviewed);
        DecisionInputs {
            decision_key: format!("local:{}", self.tree),
            evidence_key: self.run_key.to_string(),
            policy: self.policy,
            pr: PullRequest {
                base_ref: self.base_ref.to_owned(),
                open_prs_on_head: 1,
                tier,
                // The decision input owns the classes; the tiering keeps its own.
                classes: self.tiering.classes.clone(),
                owner_approval: OwnerApproval::NotApproved,
                authorship: Authorship::Other,
                readiness: Readiness::Ready,
                requeues: 0,
            },
            gates,
            weigh_in: weigh_in(self.plugins),
            llm: review.llm,
            coordinator: support(tier == Tier::Full && !review.findings.is_empty()),
            verifier: support(
                matches!(tier, Tier::Full | Tier::Lite)
                    && review
                        .findings
                        .iter()
                        .any(|finding| finding.severity >= Severity::Major),
            ),
            findings: review.findings,
            dispositions: Vec::new(),
            completed_roles: review.completed,
            degraded: review.degraded,
            waivers: Vec::new(),
            override_record: None,
        }
    }
}

/// The review's part of the inputs.
struct Review {
    llm: LlmStage,
    findings: Vec<ReviewedFinding>,
    completed: BTreeSet<RequiredRole>,
    degraded: Vec<DegradedReport>,
}

impl Review {
    fn of(reviewed: Option<&(LocalRole, Reviewed)>) -> Self {
        let none = Self {
            llm: LlmStage::NotRun {
                reason: NotRunReason::NoLlmTier,
            },
            findings: Vec::new(),
            completed: BTreeSet::new(),
            degraded: Vec::new(),
        };
        let Some((role, reviewed)) = reviewed else {
            return none;
        };
        match &reviewed.stage {
            Stage::Completed(findings) => Self {
                llm: LlmStage::Completed,
                findings: findings
                    .iter()
                    .map(|valid| ReviewedFinding {
                        fp: valid.fingerprint,
                        // Each finding is its own group: no coordinator groups them locally.
                        group_id: valid.fingerprint.to_string(),
                        role: role.bundle().code().to_owned(),
                        severity: valid.finding.severity,
                        pre_existing: valid.finding.pre_existing,
                        verdict: None,
                    })
                    .collect(),
                completed: BTreeSet::from([role.required()]),
                degraded: Vec::new(),
            },
            Stage::Degraded(failure) => Self {
                llm: LlmStage::Completed,
                degraded: vec![DegradedReport {
                    role: StageRole::from(role.required()),
                    reason: *failure,
                }],
                ..none
            },
            Stage::Unavailable => Self {
                llm: LlmStage::Deferred {
                    reason: Deferral::InfraLlmUnavailable,
                },
                ..none
            },
        }
    }
}

/// A coordinator or verifier: never run locally, so failed when it was `due`.
const fn support(due: bool) -> SupportOutcome {
    if due {
        SupportOutcome::Failed {
            reason: CallFailure::Unavailable,
        }
    } else {
        SupportOutcome::NotNeeded
    }
}

/// The Weigh-In gate does not run locally: when base config requires it, there is no evidence.
fn weigh_in(plugins: &PluginsConfig) -> WeighIn {
    let required = plugins
        .plugins()
        .iter()
        .any(|plugin| plugin.required() && matches!(plugin.adapter(), Adapter::WeighIn { .. }));
    if required {
        WeighIn::NoEvidence
    } else {
        WeighIn::NotRequired
    }
}
