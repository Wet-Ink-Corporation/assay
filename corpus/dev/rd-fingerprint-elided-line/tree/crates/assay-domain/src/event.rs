//! The `assay.event/1` envelope and its kinds (`docs/architecture/03-contracts.md` §6).
//!
//! Each job appends events to its own JSONL stream, ordered by `(job, idx)`; there is no global
//! sequence. The streams are an audit record, not an event-sourced aggregate.

mod payload;

use serde::{Deserialize, Serialize};

use crate::run_uid::RunUid;
use crate::vocabulary::closed_enum;

pub use payload::{
    EventPayload, FeedbackSignal, GateRun, Preflight, RateLimitStatus, SpecialistRun,
    VerifierVerdict, VoidReason,
};

closed_enum! {
    /// The `schema` tag of an event; a closed set of the versions this build reads.
    #[derive(Default)]
    pub enum EventSchema {
        /// `assay.event/1`.
        #[default]
        V1 => "assay.event/1",
    }
}

/// One line of a job's event stream (03 §6): the envelope, with the kind and its data in
/// [`payload`](Self::payload).
///
/// On the wire `kind` and `data` sit beside the envelope fields; unknown fields are rejected
/// at every level, as the schema rejects them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// The contract version.
    pub schema: EventSchema,
    /// The job whose stream this event belongs to.
    pub job: String,
    /// Position in that job's stream.
    pub idx: u64,
    /// When it happened, as an RFC 3339 date-time; the schema checks the format on read.
    pub ts: String,
    /// The run's evidence key.
    pub run_key: String,
    /// The workflow run that emitted it.
    pub run_uid: RunUid,
    /// The pipeline stage (`S0`, `S2b`, ...).
    pub stage: String,
    /// What happened: `kind` and `data` on the wire.
    #[serde(flatten)]
    pub payload: EventPayload,
}

closed_enum! {
    /// The kind of an event, without its data: the `kind` field of `assay.event/1` (03 §6).
    pub enum EventKind {
        /// See [`EventPayload::RunRequested`].
        RunRequested => "RunRequested",
        /// See [`EventPayload::PlanComputed`].
        PlanComputed => "PlanComputed",
        /// See [`EventPayload::Admitted`].
        Admitted => "Admitted",
        /// See [`EventPayload::PreflightPassed`].
        PreflightPassed => "PreflightPassed",
        /// See [`EventPayload::PreflightFailed`].
        PreflightFailed => "PreflightFailed",
        /// See [`EventPayload::GateStarted`].
        GateStarted => "GateStarted",
        /// See [`EventPayload::GateFinished`].
        GateFinished => "GateFinished",
        /// See [`EventPayload::SpecialistStarted`].
        SpecialistStarted => "SpecialistStarted",
        /// See [`EventPayload::SpecialistFinished`].
        SpecialistFinished => "SpecialistFinished",
        /// See [`EventPayload::SpecialistFailed`].
        SpecialistFailed => "SpecialistFailed",
        /// See [`EventPayload::SpecialistBudgetExhausted`].
        SpecialistBudgetExhausted => "SpecialistBudgetExhausted",
        /// See [`EventPayload::CoordinatorFinished`].
        CoordinatorFinished => "CoordinatorFinished",
        /// See [`EventPayload::StageDegraded`].
        StageDegraded => "StageDegraded",
        /// See [`EventPayload::RateLimitObserved`].
        RateLimitObserved => "RateLimitObserved",
        /// See [`EventPayload::FindingRaised`].
        FindingRaised => "FindingRaised",
        /// See [`EventPayload::FindingVerified`].
        FindingVerified => "FindingVerified",
        /// See [`EventPayload::FindingDisposed`].
        FindingDisposed => "FindingDisposed",
        /// See [`EventPayload::FindingPublished`].
        FindingPublished => "FindingPublished",
        /// See [`EventPayload::DecisionMade`].
        DecisionMade => "DecisionMade",
        /// See [`EventPayload::CheckPublished`].
        CheckPublished => "CheckPublished",
        /// See [`EventPayload::BundlePersisted`].
        BundlePersisted => "BundlePersisted",
        /// See [`EventPayload::WaiverApplied`].
        WaiverApplied => "WaiverApplied",
        /// See [`EventPayload::OverrideApplied`].
        OverrideApplied => "OverrideApplied",
        /// See [`EventPayload::FeedbackReceived`].
        FeedbackReceived => "FeedbackReceived",
        /// See [`EventPayload::RunSuperseded`].
        RunSuperseded => "RunSuperseded",
        /// See [`EventPayload::BudgetDeferred`].
        BudgetDeferred => "BudgetDeferred",
        /// See [`EventPayload::SlotBusy`].
        SlotBusy => "SlotBusy",
        /// See [`EventPayload::LedgerWritten`].
        LedgerWritten => "LedgerWritten",
        /// See [`EventPayload::SlotReleased`].
        SlotReleased => "SlotReleased",
        /// See [`EventPayload::SlotReaped`].
        SlotReaped => "SlotReaped",
        /// See [`EventPayload::PushRecorded`].
        PushRecorded => "PushRecorded",
        /// See [`EventPayload::CommandRecorded`].
        CommandRecorded => "CommandRecorded",
        /// See [`EventPayload::CommandRejected`].
        CommandRejected => "CommandRejected",
        /// See [`EventPayload::CommandVoided`].
        CommandVoided => "CommandVoided",
        /// See [`EventPayload::RetryGranted`].
        RetryGranted => "RetryGranted",
        /// See [`EventPayload::EvidenceReused`].
        EvidenceReused => "EvidenceReused",
    }
}
