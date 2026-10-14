//! assay domain: findings, severities, fingerprints, rubric, tiers, the run state machine and
//! its events.
//!
//! Pure: no I/O, no clock, no environment, no async runtime. Fingerprints are computed here and
//! only here (`docs/architecture/02-architecture.md` §2).
//!
//! The versioned contract types (`assay.findings/1`, `assay.event/1`) derive serde with their
//! wire names pinned; every closed vocabulary of `docs/architecture/03-contracts.md` exposes
//! `ALL` and its wire `code()`.

mod vocabulary;

mod aeu;
mod content_hash;
mod decide;
mod decision;
mod dismissal;
mod disposition;
mod event;
mod finding;
mod findings;
mod fingerprint;
mod normalise;
mod reason;
mod rubric;
mod run_uid;
mod severity;
mod tier;
mod tiering;

pub use aeu::Aeu;
pub use content_hash::ContentHash;
pub use decide::decide;
pub use decide::inputs::{
    Authorship, DecisionInputs, Deferral, GateFinding, GateOutcome, GateReport, InfraPolicy,
    LlmPolicy, LlmStage, NotRunReason, Override, OverrideScope, OwnerApproval, Policy, PullRequest,
    Readiness, ReviewedFinding, StepAttribution, SupportOutcome, Waiver, WeighIn,
};
pub use decision::{
    Conclusion, Counts, Decision, DecisionSchema, DegradedRole, FindingDisposition, Reason, Waived,
};
pub use dismissal::{Dismissal, DismissalMatch};
pub use disposition::Disposition;
pub use event::{
    Event, EventKind, EventPayload, EventSchema, FeedbackSignal, GateRun, Preflight,
    RateLimitStatus, SpecialistRun, VerifierVerdict, VoidReason,
};
pub use finding::{Caller, Class, Confidence, Finding, FindingKind, IntroducedBy, Side};
pub use findings::{FindingsDocument, FindingsSchema, GateConclusion, Source, SourceKind, Stats};
pub use fingerprint::{Anchor, Fingerprint, ParseFingerprintError};
pub use normalise::normalise;
pub use reason::ReasonCode;
pub use rubric::{Rubric, RubricError};
pub use run_uid::RunUid;
pub use severity::{Marker, Severity};
pub use tier::{PrClass, Tier};
pub use tiering::{
    BaselineChange, ChangeSet, ExecCause, ExecRoot, FileChange, FileKind, FilteredStats, Glob,
    GlobError, Lines, ManifestKey, ManifestTrustChange, PackageChange, PackageDiff, RepoGlobs,
    RequiredRole, TierReason, Tiering, tier,
};
