//! What `decide` reads: validated, Z4-recomputed facts about one PR head
//! (`docs/architecture/04-pipeline.md` §5 "Inputs").
//!
//! These are a projection the application builds from the gate documents, the verified
//! findings, the recomputed plan, the GitHub state snapshot and base policy. They are plain data:
//! validation, fingerprinting, eligibility and command-record checks happen before `decide`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::decision::FindingDisposition;
use crate::event::VerifierVerdict;
use crate::findings::GateConclusion;
use crate::fingerprint::Fingerprint;
use crate::reason::ReasonCode;
use crate::severity::Severity;
use crate::tier::{PrClass, Tier};
use crate::tiering::RequiredRole;
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
    /// Specialist roles that returned structured output, on every shard (04 §3). When the stage
    /// completed, a role the PR's tier and classes require that is not here, or is also in
    /// `degraded`, degrades the stage (rules 8b, 8c). A set, so order and repetition in the
    /// serialised inputs never change the decision or its digest.
    pub completed_roles: BTreeSet<RequiredRole>,
    /// How the coordinator ran. On a `full` PR with reviewer findings it must have completed.
    pub coordinator: SupportOutcome,
    /// How the blind verifier ran; `NotNeeded` only when there was no candidate. On `full` or
    /// `lite`, a blocker or major is a candidate (04 §3), so it must not be `NotNeeded` then.
    pub verifier: SupportOutcome,
    /// Roles reported degraded, each with its reason: own cap, turns, timeout, malformed output
    /// after a retry, or no model to call (rules 8b, 8c). A role degraded on any shard is
    /// degraded.
    pub degraded: Vec<DegradedReport>,
    /// Valid waiver command records for this head.
    pub waivers: Vec<Waiver>,
    /// The valid override command record for this head, if any.
    pub override_record: Option<Override>,
}

/// A role of the LLM stage that reported itself degraded (rules 8b, 8c).
///
/// The role is a [`StageRole`] and the reason a [`CallFailure`], never free text, so a report
/// with an empty or unknown role or reason is refused where the inputs are parsed rather than
/// reaching the decision's `degraded[]`, whose `role` and `reason` `assay.decision/1` requires
/// to be non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DegradedReport {
    /// The role: a specialist (`bugs`, `combined`), `coordinator` or `verifier`.
    pub role: StageRole,
    /// Why it produced nothing.
    pub reason: CallFailure,
}

closed_enum! {
    /// A call of the LLM stage that can report itself degraded (04 §3): a specialist of the
    /// roster, the coordinator or the verifier.
    pub enum StageRole {
        /// The `bugs` specialist (`full`).
        Bugs => "bugs",
        /// The `security` specialist (`full`).
        Security => "security",
        /// The `design` specialist (`full`).
        Design => "design",
        /// The `meta` specialist (`full`, trust path or exec surface).
        Meta => "meta",
        /// The `combined` specialist (`lite`).
        Combined => "combined",
        /// The `combined-micro` specialist (`micro`).
        CombinedMicro => "combined-micro",
        /// The coordinator (`full`).
        Coordinator => "coordinator",
        /// The blind verifier.
        Verifier => "verifier",
    }
}

impl From<RequiredRole> for StageRole {
    fn from(role: RequiredRole) -> Self {
        match role {
            RequiredRole::Bugs => Self::Bugs,
            RequiredRole::Security => Self::Security,
            RequiredRole::Design => Self::Design,
            RequiredRole::Meta => Self::Meta,
            RequiredRole::Combined => Self::Combined,
            RequiredRole::CombinedMicro => Self::CombinedMicro,
        }
    }
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
    /// The recomputed tier (rule 3), after Z4 accepts `admit`'s full-to-lite budget degrade
    /// (R3-27). With `classes` it names the roles rule 8 requires (04 §3).
    pub tier: Tier,
    /// The recomputed classes (rules 4, 8).
    pub classes: Vec<PrClass>,
    /// `owner_approved(head)`, computed by the eligibility rule (rule 4).
    pub owner_approval: OwnerApproval,
    /// Whether an agent identity authored the PR (rule 8a, `slot-busy`).
    pub authorship: Authorship,
    /// The PR's draft flag from Z4's GitHub state snapshot taken at decide time (04 §5 inputs,
    /// 03 §2.1). It is independent of the plan whose `draft` yields
    /// [`NotRunReason::Draft`]: that excuse holds only while this still reads `Draft`. A
    /// decide-only re-decision keeps the evidence bundle's flag (#61 D15, wi-f4455d): marking
    /// the PR ready runs the pipeline again, whose `open` stamps the check `pending:`, and a
    /// re-decision never writes over a check carrying another run's stamp.
    pub readiness: Readiness,
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

closed_enum! {
    /// Whether the PR is a draft, as GitHub reported it in the snapshot taken at decide time
    /// (see [`PullRequest::readiness`]).
    pub enum Readiness {
        /// A draft: the LLM stage runs on `ready_for_review` (04 §2.4).
        Draft => "draft",
        /// Ready for review.
        Ready => "ready",
    }
}

/// One gate of the plan and what it reported.
///
/// A failed gate is cleared by waivers only when its adapter attested that every cause of the
/// failure is a finding ([`StepAttribution::Known`] with no failed step) and every finding is a
/// waived human-decision finding. A gate whose steps are [`StepAttribution::Unknown`] (its
/// document attests none, or its adapter is not temper, 03 §2.1) or that names a failed step is
/// never cleared.
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
        /// What the adapter attests about failures with no finding behind them.
        steps: StepAttribution,
    },
    /// Bad exit, signal, timeout, or a missing or invalid document.
    Error {
        /// What went wrong (`gitattributes`, `timeout`).
        detail: String,
    },
    /// A required gate did not run; never a pass.
    Skipped,
}

/// Whether a gate's adapter attested which of its steps failed without a finding (03 §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum StepAttribution {
    /// The adapter did not attest: some failure may have no finding, so waivers never clear
    /// the gate. What every gate but a temper gate whose document carries `failed_steps` is.
    Unknown,
    /// The steps (`fmt`, `deny`, `tests`) that failed without a finding, from the temper
    /// document's `failed_steps` (#153); empty means every failure cause is a finding. Waivers
    /// clear the gate only when it is empty.
    Known(Vec<String>),
}

impl StepAttribution {
    /// Whether the adapter attested that every failure cause is a finding.
    #[must_use]
    pub fn all_failures_are_findings(&self) -> bool {
        matches!(self, Self::Known(steps) if steps.is_empty())
    }
}

/// One gate finding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateFinding {
    /// The fingerprint Z4 computed.
    pub fp: Fingerprint,
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
    /// The stage did not run. Only a tier without one (`trivial`, `needs-split`) or a draft
    /// excuses that; on any other PR `decide` treats it as
    /// `Deferred { reason: InfraLlmUnavailable }`.
    NotRun {
        /// Why it did not run.
        reason: NotRunReason,
    },
    /// The stage completed; required roles may still have degraded.
    Completed,
    /// The stage did not run, for the given reason.
    Deferred {
        /// Why.
        reason: Deferral,
    },
}

impl LlmStage {
    /// The stage as rule 8 reads it. On a PR whose tier has an LLM stage (`full`, `lite`,
    /// `micro`), a stage that did not run is an unavailable LLM unless the PR really is a draft,
    /// so nothing downgrades silently: `NoLlmTier` contradicts the tier, and `Draft` must match
    /// the PR's `readiness`.
    #[must_use]
    pub const fn effective(self, pr: &PullRequest) -> Self {
        let reviewed_tier = matches!(pr.tier, Tier::Full | Tier::Lite | Tier::Micro);
        let excused = match self {
            Self::NotRun {
                reason: NotRunReason::Draft,
            } => matches!(pr.readiness, Readiness::Draft),
            Self::NotRun {
                reason: NotRunReason::NoLlmTier,
            } => !reviewed_tier,
            Self::Completed | Self::Deferred { .. } => true,
        };
        if reviewed_tier && !excused {
            Self::Deferred {
                reason: Deferral::InfraLlmUnavailable,
            }
        } else {
            self
        }
    }
}

/// How a support call of the LLM stage, the coordinator or the verifier, ran (04 §3).
///
/// A `Failed` call always degrades the stage (rules 8b, 8c): on a PR of a blocking class or
/// policy it is `required-role-degraded`, which waiting cannot heal, never a deferral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SupportOutcome {
    /// No call was due: no candidate for the verifier, no coordinator on `lite` or `micro`.
    NotNeeded,
    /// The call returned structured output.
    Completed,
    /// The call failed or was unavailable.
    Failed {
        /// Why.
        reason: CallFailure,
    },
}

closed_enum! {
    /// Why a call of the LLM stage produced nothing: the degraded outcomes of 04 §5 rule 8b and
    /// the model runner port (03 §7), plus no model to call.
    pub enum CallFailure {
        /// It exhausted its own budget cap (`BudgetExhausted`).
        Cap => "cap",
        /// It exhausted its turns (`TurnsExhausted`).
        Turns => "turns",
        /// It timed out.
        Timeout => "timeout",
        /// Its output was malformed after the one retry.
        Malformed => "malformed",
        /// No model could be called (quota or authentication lost mid-stage).
        Unavailable => "unavailable",
    }
}

closed_enum! {
    /// Why the LLM stage did not run without being deferred.
    pub enum NotRunReason {
        /// The plan says the tier has no LLM stage (`trivial`, `needs-split`).
        NoLlmTier => "no-llm-tier",
        /// The plan (`assay.run/1` `draft`) says the PR is a draft: gates only until
        /// `ready_for_review` (04 §2.4). Excused only while [`PullRequest::readiness`], read at
        /// decide time, still says `Draft`.
        Draft => "draft",
    }
}

closed_enum! {
    /// Why the LLM stage did not run (04 §5 rule 8a).
    pub enum Deferral {
        /// The day's budget is spent.
        BudgetDeferred => "budget-deferred",
        /// The LLM is unavailable before the stage could run (preflight failure, an unreadable
        /// ledger). A coordinator or verifier that fails once the stage runs is a
        /// [`SupportOutcome::Failed`] instead.
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
    pub fp: Fingerprint,
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
    pub fp: Fingerprint,
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
