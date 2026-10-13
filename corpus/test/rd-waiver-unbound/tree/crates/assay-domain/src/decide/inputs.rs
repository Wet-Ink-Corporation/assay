//! What `decide` reads: validated, Z4-recomputed facts about one PR head
//! (`docs/architecture/04-pipeline.md` §5 "Inputs").
//!
//! These are a projection the application builds from the gate documents, the verified
//! findings, the recomputed plan, the GitHub state snapshot and base policy. They are plain data:
//! validation, fingerprinting, eligibility and command-record checks happen before `decide`.

use serde::{Deserialize, Serialize};

use crate::decision::{DegradedRole, FindingDisposition};
use crate::event::VerifierVerdict;
use crate::findings::GateConclusion;
use crate::reason::ReasonCode;
use crate::severity::Severity;
use crate::tier::{PrClass, Tier};
use crate::vocabulary::closed_enum;

/// Everything one decision depends on besides the rubric.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionInputs {
    /// The decision's idempotency key (02 §5), copied into the decision.
    pub decision_key: String,
    /// The evidence key (02 §5), copied into the decision.
    pub evidence_key: String,
    /// Policy from base `.assay/plugins.toml`.
    pub policy: Policy,
    /// Facts about the PR and its head.
    pub pr: PullRequest,
    /// Every gate of the plan, the Weigh-In gate included.
    pub gates: Vec<GateReport>,
    /// What the Weigh-In gate reports (rule 5).
    pub weigh_in: WeighIn,
    /// Whether and how the LLM stage ran.
    pub llm: LlmStage,
    /// Reviewer findings after validation and verification.
    pub findings: Vec<ReviewedFinding>,
    /// Dispositions recorded upstream (validation, coordinator, verifier).
    pub dispositions: Vec<FindingDisposition>,
    /// Required roles that ran degraded (rule 8b, 8c).
    pub degraded: Vec<DegradedRole>,
    /// Valid waiver command records for this head.
    pub waivers: Vec<Waiver>,
    /// The valid override command record for this head, if any.
    pub override_record: Option<Override>,
}

/// Base policy (03 §1 `[assay]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Bases the verdict check is required on (rule 2).
    pub protected_bases: Vec<String>,
    /// Whether a verified blocker fails the PR (rule 6).
    pub llm_blockers: LlmPolicy,
    /// Whether too many verified majors fail the PR (rule 7).
    pub llm_majors: LlmPolicy,
    /// What an unavailable LLM stage does (rule 8).
    pub on_infra_failure: InfraPolicy,
    /// PR classes that always block when the LLM stage is unavailable (rule 8).
    pub block_on_infra_for: Vec<PrClass>,
}

closed_enum! {
    /// Whether an LLM rule may fail a PR (`llm_blockers`, `llm_majors`; promotion by QA-05).
    pub enum LlmPolicy {
        /// Published, never failing (the default).
        Advisory => "advisory",
        /// Fails the PR.
        Block => "block",
    }
}

closed_enum! {
    /// What an unavailable LLM stage does (`on_infra_failure`); gates are never affected.
    pub enum InfraPolicy {
        /// Success with a visible `llm-degraded` warning.
        Warn => "warn",
        /// Pending or failure, as for a trust-path PR.
        Block => "block",
    }
}

/// Facts about the PR, recomputed by Z4.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    /// The base ref the PR targets (rule 2).
    pub base_ref: String,
    /// Open PRs on `head_sha` into this protected base, this one included (rule 2a).
    pub open_prs_on_head: u32,
    /// The recomputed tier (rule 3).
    pub tier: Tier,
    /// The recomputed classes (rules 4, 8).
    pub classes: Vec<PrClass>,
    /// `owner_approved(head)`, computed by the eligibility rule (rule 4).
    pub owner_approval: OwnerApproval,
    /// Whether an agent identity authored the PR (rule 8a, `slot-busy`).
    pub authorship: Authorship,
    /// How many times this head's deferred run was already requeued (rule 8a).
    pub requeues: u32,
}

closed_enum! {
    /// The result of `owner_approved(head)` (04 §5), computed elsewhere.
    pub enum OwnerApproval {
        /// An eligible owner or approver approved this head after the push clock.
        Approved => "approved",
        /// No eligible approval.
        NotApproved => "not-approved",
    }
}

closed_enum! {
    /// Who authored the PR, by numeric identity (04 §2.5).
    pub enum Authorship {
        /// An identity listed in base `.assay/agents`.
        Agent => "agent",
        /// Anyone else.
        Other => "other",
    }
}

/// One gate of the plan and what it reported.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateReport {
    /// The plugin name (`temper`).
    pub name: String,
    /// Whether base config marks it required (rule 1 reads only required gates).
    pub required: bool,
    /// Whether it ran and how.
    pub outcome: GateOutcome,
    /// Its findings, fingerprinted by Z4.
    pub findings: Vec<GateFinding>,
}

/// How a gate ended (03 §2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GateOutcome {
    /// It ran; its document's conclusion is authoritative.
    Ran {
        /// The gate's conclusion.
        conclusion: GateConclusion,
    },
    /// Bad exit, signal, timeout, or a missing or invalid document.
    Error {
        /// What went wrong (`gitattributes`, `timeout`).
        detail: String,
    },
    /// A required gate did not run; never a pass.
    Skipped,
}

/// One gate finding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateFinding {
    /// The fingerprint Z4 computed.
    // A plain string until the `Fingerprint` type of #49 lands.
    pub fp: String,
    /// The plugin's rule id; waivers read it.
    pub rule: Option<String>,
    /// The display severity the adapter mapped.
    pub severity: Severity,
}

closed_enum! {
    /// What the Weigh-In gate reports (04 §9).
    pub enum WeighIn {
        /// The Weigh-In gate is not required, so rule 5 never fires.
        NotRequired => "not-required",
        /// Required, and nothing blocks.
        Clear => "clear",
        /// Required, with an open blocking item.
        OpenBlocking => "open-blocking",
        /// Required, with items awaiting triage.
        PendingTriage => "pending-triage",
        /// Required, and it holds no evidence for the change.
        NoEvidence => "no-evidence",
    }
}

/// Whether and how the LLM stage ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LlmStage {
    /// The tier has no LLM stage, or the PR is a draft.
    NotRun,
    /// The stage completed; required roles may still have degraded.
    Completed,
    /// The stage did not run, for the given reason.
    Deferred {
        /// Why.
        reason: Deferral,
    },
}

closed_enum! {
    /// Why the LLM stage did not run (04 §5 rule 8a).
    pub enum Deferral {
        /// The day's budget is spent.
        BudgetDeferred => "budget-deferred",
        /// The LLM is unavailable: preflight, verifier or coordinator failure.
        InfraLlmUnavailable => "infra-llm-unavailable",
        /// No semaphore slot was free.
        SlotBusy => "slot-busy",
    }
}

impl Deferral {
    /// The reason code a deferral is published under.
    #[must_use]
    pub const fn reason_code(self) -> ReasonCode {
        match self {
            Self::BudgetDeferred => ReasonCode::BudgetDeferred,
            Self::InfraLlmUnavailable => ReasonCode::InfraLlmUnavailable,
            Self::SlotBusy => ReasonCode::SlotBusy,
        }
    }
}

/// One reviewer finding after validation, coordination and verification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedFinding {
    /// The fingerprint Z4 computed.
    // A plain string until the `Fingerprint` type of #49 lands.
    pub fp: String,
    /// The coordinator's root-cause group; majors are counted by distinct group.
    pub group_id: String,
    /// The role that raised it.
    pub role: String,
    /// Its severity after verification (a refuted finding is already `minor`).
    pub severity: Severity,
    /// Whether Z4 confirmed it is about code the diff did not introduce.
    pub pre_existing: bool,
    /// The verifier's verdict; none when no verifier ran (micro, or over the cap).
    pub verdict: Option<VerifierVerdict>,
}

/// A valid `/assay waive <finding-id>` command record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waiver {
    /// The finding it names.
    pub fp: String,
    /// The eligible owner who issued it.
    pub by: String,
}

/// A valid `/assay override llm|all <reasons_digest> <reason>` command record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    /// What it neutralises.
    pub scope: OverrideScope,
    /// The reasons digest it quotes; it is void unless the current reasons hash to it.
    pub reasons_digest: String,
    /// The eligible owner who issued it.
    pub by: String,
}

closed_enum! {
    /// What an override neutralises.
    pub enum OverrideScope {
        /// Only rule 8 (an unavailable or degraded LLM stage).
        Llm => "llm",
        /// Every rule: the break-glass.
        All => "all",
    }
}
