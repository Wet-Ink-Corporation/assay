//! Reason codes: the cause of a check conclusion (`docs/architecture/03-contracts.md` §5).

use crate::vocabulary::closed_enum;

closed_enum! {
    /// Why a check concluded as it did: `assay.decision/1` `reasons[].code`. A closed set (03 §5).
    pub enum ReasonCode {
        /// A required gate reported `fail`.
        GateFailed => "gate-failed",
        /// A gate errored: bad exit, signal, timeout or a missing or invalid document (03 §2).
        GateError => "gate-error",
        /// A required gate did not run; never a pass.
        GateSkipped => "gate-skipped",
        /// The verifier confirmed a blocker.
        BlockerVerified => "blocker-verified",
        /// More majors than the rubric allows.
        MajorsOverThreshold => "majors-over-threshold",
        /// Weigh-In has an open blocking item.
        WeighInOpenBlocking => "weigh-in-open-blocking",
        /// Weigh-In holds no evidence for the change.
        WeighInNoEvidence => "weigh-in-no-evidence",
        /// Weigh-In items await triage.
        WeighInPendingTriage => "weigh-in-pending-triage",
        /// The change is too large to review; split it.
        NeedsSplit => "needs-split",
        /// The LLM stage could not run.
        InfraLlmUnavailable => "infra-llm-unavailable",
        /// The LLM stage ran degraded.
        LlmDegraded => "llm-degraded",
        /// A required role hit its own cap, which waiting cannot heal.
        RequiredRoleDegraded => "required-role-degraded",
        /// A trust-path change lacks owner approval.
        TrustPathUnapproved => "trust-path-unapproved",
        /// The recomputed plan differs from the one the run used.
        PlanMismatch => "plan-mismatch",
        /// Publishing to GitHub failed.
        PublishError => "publish-error",
        /// Persisting the bundle failed.
        PersistError => "persist-error",
        /// The evidence bundle expired.
        BundleExpired => "bundle-expired",
        /// An owner override applies; its scope `llm|all` is in `detail`.
        Override => "override",
        /// The run was deferred for budget.
        BudgetDeferred => "budget-deferred",
        /// No semaphore slot was free.
        SlotBusy => "slot-busy",
        /// The base is not protected, so the check is not required there.
        BaseNotProtected => "base-not-protected",
        /// The head commit is ambiguous.
        AmbiguousHead => "ambiguous-head",
        /// A completed check could not be reopened, so `open` published this fail-safe state.
        ReDeciding => "re-deciding",
    }
}
