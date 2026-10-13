//! What an event records: its kind and the `data` that kind carries (03 §6).

use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use super::EventKind;
use crate::disposition::Disposition;
use crate::findings::GateConclusion;
use crate::fingerprint::Fingerprint;
use crate::run_uid::RunUid;
use crate::severity::Severity;
use crate::vocabulary::closed_enum;

closed_enum! {
    /// The rate-limit states assay records; `allowed` fires routinely and is not recorded
    /// (03 §8).
    pub enum RateLimitStatus {
        /// Allowed, close to the limit.
        AllowedWarning => "allowed_warning",
        /// Rejected by the limit.
        Rejected => "rejected",
    }
}

closed_enum! {
    /// The verifier's judgement of a finding.
    pub enum VerifierVerdict {
        /// The finding holds.
        Confirmed => "confirmed",
        /// The finding is wrong.
        Refuted => "refuted",
        /// The verifier could not tell.
        Unclear => "unclear",
    }
}

closed_enum! {
    /// A feedback signal on a published finding.
    pub enum FeedbackSignal {
        /// Someone reacted to the comment.
        Reaction => "reaction",
        /// The owner dismissed the finding.
        Dismiss => "dismiss",
        /// The PR author dismissed their own finding.
        SelfDismiss => "self-dismiss",
        /// The thread was resolved.
        Resolved => "resolved",
        /// The anchored code changed.
        AnchorChanged => "anchor-changed",
    }
}

closed_enum! {
    /// Why a recorded command no longer counts.
    pub enum VoidReason {
        /// The comment was edited.
        Edited => "edited",
        /// The comment was deleted.
        Deleted => "deleted",
        /// The comment no longer matches its recorded hash.
        Hash => "hash",
    }
}

/// The data of `PreflightPassed` and `PreflightFailed`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preflight {
    /// How the CLI authenticated (`claude auth status`'s `authMethod`), or `unknown` when the
    /// preflight failed before reading it.
    pub auth_method: String,
    /// The CLI version found, or `unknown` when the preflight failed before reading it.
    pub cli_version: String,
    /// Whether the `system/init` message was clean: no MCP server, the three read-only tools,
    /// and only built-in plugins, skills and agents on the allowlist pinned next to the CLI
    /// version (03 §8).
    pub init_clean: bool,
    /// The plugins, skills and agents the `system/init` message listed, as observed; absent
    /// when the message could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtins: Option<BuiltinSet>,
}

/// The plugins, skills and agents a Claude Code session loaded, by name, as its `system/init`
/// message listed them (03 §8). On a clean session these are the CLI's built-ins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinSet {
    /// Plugin (mod) names.
    pub plugins: Vec<String>,
    /// Skill names.
    pub skills: Vec<String>,
    /// Agent type names.
    pub agents: Vec<String>,
}

/// The data of `GateStarted` and `GateFinished`; only `plugin` is required, the rest describes
/// a finished gate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateRun {
    /// The gate plugin.
    pub plugin: String,
    /// Its exit status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i64>,
    /// Its conclusion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<GateConclusion>,
    /// How long it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// The data of the `Specialist*` kinds; only `role` is required, the rest describes a call that
/// ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecialistRun {
    /// The reviewer role.
    pub role: String,
    /// The model it ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Tokens read from the prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Cost in AEU.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_aeu: Option<f64>,
    /// Wall-clock duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Turns taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u64>,
    /// Why the call stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

/// An event's kind with the `data` it carries (03 §6). Kinds whose data 03 lists no fields for
/// carry an empty object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
pub enum EventPayload {
    /// A run was requested.
    RunRequested {},
    /// `plan` computed the run description.
    PlanComputed {},
    /// `admit` admitted the run.
    Admitted {
        /// The semaphore slot taken.
        slot: String,
        /// AEU reserved for the run.
        reserved_aeu: f64,
    },
    /// The model runner's preflight passed.
    PreflightPassed(Preflight),
    /// The model runner's preflight failed.
    PreflightFailed(Preflight),
    /// A gate started.
    GateStarted(GateRun),
    /// A gate finished.
    GateFinished(GateRun),
    /// A specialist role started.
    SpecialistStarted(SpecialistRun),
    /// A specialist role finished.
    SpecialistFinished(SpecialistRun),
    /// A specialist role failed.
    SpecialistFailed(SpecialistRun),
    /// A specialist role ran out of budget.
    SpecialistBudgetExhausted(SpecialistRun),
    /// The coordinator finished.
    CoordinatorFinished {},
    /// A stage ran degraded.
    StageDegraded {
        /// The stage.
        stage: String,
        /// Why.
        reason: String,
    },
    /// The CLI reported a rate-limit state worth recording.
    RateLimitObserved {
        /// The state.
        status: RateLimitStatus,
        /// When the limit resets, as an RFC 3339 date-time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<String>,
    },
    /// A finding was raised.
    FindingRaised {},
    /// The verifier judged a finding.
    FindingVerified {
        /// Its judgement.
        verdict: VerifierVerdict,
    },
    /// A finding received a disposition, possibly changing its severity.
    FindingDisposed {
        /// The disposition.
        code: Disposition,
        /// The severity before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Severity>,
        /// The severity after.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Severity>,
    },
    /// A finding was published as a comment.
    FindingPublished {
        /// The comment's URL.
        url: String,
    },
    /// `decide` made its decision.
    DecisionMade {},
    /// The check run was published.
    CheckPublished {},
    /// The evidence bundle was persisted.
    BundlePersisted {
        /// Its SHA-256, lowercase hex.
        sha256: String,
    },
    /// An owner waiver was applied to a finding.
    WaiverApplied {
        /// Who waived it.
        who: String,
        /// The finding's fingerprint.
        fp: Fingerprint,
    },
    /// An owner override was applied.
    OverrideApplied {
        /// Who overrode.
        who: String,
        /// Why.
        reason: String,
    },
    /// Feedback arrived on a published finding.
    FeedbackReceived {
        /// The signal.
        signal: FeedbackSignal,
    },
    /// A newer run superseded this one; it writes no check and no comment.
    RunSuperseded {},
    /// The run was deferred for budget.
    BudgetDeferred {},
    /// No semaphore slot was free.
    SlotBusy {},
    /// The ledger record was written.
    LedgerWritten {},
    /// The run's slot was released.
    SlotReleased {},
    /// A stale lease was reaped.
    SlotReaped {
        /// The run that held it.
        run_uid: RunUid,
    },
    /// A push to the PR was recorded.
    PushRecorded {
        /// The head before, 40 lowercase hex.
        before: String,
        /// The head after, 40 lowercase hex.
        after: String,
        /// The pusher's numeric GitHub id.
        sender_id: NonZeroU64,
        /// Whether the push was forced.
        forced: bool,
    },
    /// An `/assay` command was recorded.
    CommandRecorded {
        /// The command, as written.
        command: String,
        /// The commenter's numeric GitHub id.
        actor_id: NonZeroU64,
        /// The comment's id.
        comment_id: NonZeroU64,
    },
    /// An `/assay` command was rejected.
    CommandRejected {
        /// The command, as written.
        command: String,
        /// The commenter's numeric GitHub id.
        actor_id: NonZeroU64,
        /// Why.
        reason: String,
    },
    /// A recorded command was voided.
    CommandVoided {
        /// The comment's id.
        comment_id: NonZeroU64,
        /// Why.
        reason: VoidReason,
    },
    /// A retry of a role was granted.
    RetryGranted {
        /// The role.
        role: String,
    },
    /// Evidence from an earlier run was reused.
    EvidenceReused {
        /// The reused evidence key.
        evidence_key: String,
    },
}

impl EventPayload {
    /// The kind of this event.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::RunRequested {} => EventKind::RunRequested,
            Self::PlanComputed {} => EventKind::PlanComputed,
            Self::Admitted { .. } => EventKind::Admitted,
            Self::PreflightPassed(_) => EventKind::PreflightPassed,
            Self::PreflightFailed(_) => EventKind::PreflightFailed,
            Self::GateStarted(_) => EventKind::GateStarted,
            Self::GateFinished(_) => EventKind::GateFinished,
            Self::SpecialistStarted(_) => EventKind::SpecialistStarted,
            Self::SpecialistFinished(_) => EventKind::SpecialistFinished,
            Self::SpecialistFailed(_) => EventKind::SpecialistFailed,
            Self::SpecialistBudgetExhausted(_) => EventKind::SpecialistBudgetExhausted,
            Self::CoordinatorFinished {} => EventKind::CoordinatorFinished,
            Self::StageDegraded { .. } => EventKind::StageDegraded,
            Self::RateLimitObserved { .. } => EventKind::RateLimitObserved,
            Self::FindingRaised {} => EventKind::FindingRaised,
            Self::FindingVerified { .. } => EventKind::FindingVerified,
            Self::FindingDisposed { .. } => EventKind::FindingDisposed,
            Self::FindingPublished { .. } => EventKind::FindingPublished,
            Self::DecisionMade {} => EventKind::DecisionMade,
            Self::CheckPublished {} => EventKind::CheckPublished,
            Self::BundlePersisted { .. } => EventKind::BundlePersisted,
            Self::WaiverApplied { .. } => EventKind::WaiverApplied,
            Self::OverrideApplied { .. } => EventKind::OverrideApplied,
            Self::FeedbackReceived { .. } => EventKind::FeedbackReceived,
            Self::RunSuperseded {} => EventKind::RunSuperseded,
            Self::BudgetDeferred {} => EventKind::BudgetDeferred,
            Self::SlotBusy {} => EventKind::SlotBusy,
            Self::LedgerWritten {} => EventKind::LedgerWritten,
            Self::SlotReleased {} => EventKind::SlotReleased,
            Self::SlotReaped { .. } => EventKind::SlotReaped,
            Self::PushRecorded { .. } => EventKind::PushRecorded,
            Self::CommandRecorded { .. } => EventKind::CommandRecorded,
            Self::CommandRejected { .. } => EventKind::CommandRejected,
            Self::CommandVoided { .. } => EventKind::CommandVoided,
            Self::RetryGranted { .. } => EventKind::RetryGranted,
            Self::EvidenceReused { .. } => EventKind::EvidenceReused,
        }
    }
}
