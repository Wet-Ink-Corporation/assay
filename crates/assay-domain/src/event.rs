//! The `assay.event/1` envelope and its kinds (`docs/architecture/03-contracts.md` §6).
//!
//! Each job appends events to its own JSONL stream, ordered by `(job, idx)`; there is no global
//! sequence. The streams are an audit record, not an event-sourced aggregate.

mod payload;

use serde::{Deserialize, Serialize};

use crate::disposition::Disposition;
use crate::run_uid::RunUid;
use crate::vocabulary::closed_enum;

pub use payload::{
    BuiltinSet, EventPayload, FeedbackSignal, GateRun, Preflight, RateLimitStatus, SpecialistRun,
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
/// at every level, as the schema rejects them, and so is a `FindingDisposed` that omits a
/// fingerprint its code requires ([`Disposition::requires_fingerprint`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "EventWire")]
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

/// The wire form of an [`Event`], before the checks its fields cannot state alone.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    schema: EventSchema,
    job: String,
    idx: u64,
    ts: String,
    run_key: String,
    run_uid: RunUid,
    stage: String,
    #[serde(flatten)]
    payload: EventPayload,
}

/// Why an event that matches every field's shape was refused. Crate-private: its only producer
/// is the conversion from the private wire form, and serde reports it to the reader as text
/// (#264).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum EventError {
    /// A `FindingDisposed` omits the fingerprint its code requires (#170).
    #[error("a `{}` disposition must name its finding's fingerprint", .code.code())]
    FingerprintMissing {
        /// The disposition.
        code: Disposition,
    },
}

impl TryFrom<EventWire> for Event {
    type Error = EventError;

    fn try_from(wire: EventWire) -> Result<Self, Self::Error> {
        let EventWire {
            schema,
            job,
            idx,
            ts,
            run_key,
            run_uid,
            stage,
            payload,
        } = wire;
        if let EventPayload::FindingDisposed {
            code,
            fp: None,
            from,
            ..
        } = payload
            && code.requires_fingerprint(from)
        {
            return Err(EventError::FingerprintMissing { code });
        }
        Ok(Self {
            schema,
            job,
            idx,
            ts,
            run_key,
            run_uid,
            stage,
            payload,
        })
    }
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
