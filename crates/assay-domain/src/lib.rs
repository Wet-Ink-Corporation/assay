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

mod disposition;
mod event;
mod finding;
mod findings;
mod reason;
mod run_uid;
mod severity;

pub use disposition::Disposition;
pub use event::{
    Event, EventKind, EventPayload, EventSchema, FeedbackSignal, GateRun, Preflight,
    RateLimitStatus, SpecialistRun, VerifierVerdict, VoidReason,
};
pub use finding::{Caller, Class, Confidence, Finding, FindingKind, IntroducedBy, Side};
pub use findings::{FindingsDocument, FindingsSchema, GateConclusion, Source, SourceKind, Stats};
pub use reason::ReasonCode;
pub use run_uid::RunUid;
pub use severity::{Marker, Severity};
