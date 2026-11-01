//! The decision bundle read back (`assay.run-decision/1`, 03 §6e): strictly, as
//! [`prepare_decision`](super::prepare_decision) writes it (wi-b5affa).
//!
//! The reader accepts exactly what the writer produces: every field and no other, the evidence
//! pointer of attempt 1, a trigger of the three kinds, decision inputs whose key is a digest,
//! the GitHub snapshot as the strict snapshot reader reads a narrowed one, and events of
//! decide-only's `redecide` job only, each naming the bundle's run and evidence key, numbered
//! from 0, of the five kinds a re-decision records and in their order. Nothing read is echoed
//! in its errors.

use std::num::NonZeroU64;

use assay_domain::{Decision, DecisionInputs, Event, EventPayload, Rubric, RunUid};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use super::MAX_DECISION_BUNDLE_BYTES;
use super::bundle::{DecisionTrigger, EVENT_JOB, EVENT_STAGE, MAX_DECISION_EVENTS};
use crate::config::{CommitId, Sha256Digest};
use crate::eligibility::{RecordedState, Timestamp};
use crate::publish::{CommentId, EvidencePointer};
use crate::replay::{self, BundleShape, DecisionDiff};
use crate::status::UserId;

/// Why bytes are not an `assay.run-decision/1` bundle assay wrote. Nothing read is echoed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum DecisionBundleError {
    /// The bytes are more than [`MAX_DECISION_BUNDLE_BYTES`]; they were not parsed.
    #[error("the decision bundle is larger than {MAX_DECISION_BUNDLE_BYTES} bytes")]
    TooLarge,
    /// The bytes are not `assay.run-decision/1` as assay writes it.
    #[error("the decision bundle is not assay.run-decision/1: {0}")]
    Unreadable(BundleShape),
}

/// An `assay.run-decision/1` bundle, read strictly.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionBundle {
    run_uid: RunUid,
    evidence: EvidencePointer,
    trigger: DecisionTrigger,
    inputs: DecisionInputs,
    rubric_version: String,
    decision: Value,
    events: Vec<Event>,
    recorded: RecordedState,
}

impl DecisionBundle {
    /// The decide-only run that re-decided (attempt 1).
    #[must_use]
    pub const fn run_uid(&self) -> RunUid {
        self.run_uid
    }

    /// The evidence pointer the decision was re-decided from (J10).
    #[must_use]
    pub const fn evidence(&self) -> &EvidencePointer {
        &self.evidence
    }

    /// What started the re-decision.
    #[must_use]
    pub const fn trigger(&self) -> DecisionTrigger {
        self.trigger
    }

    /// The decision inputs it decided over.
    #[must_use]
    pub const fn inputs(&self) -> &DecisionInputs {
        &self.inputs
    }

    /// The rubric version it was decided under.
    #[must_use]
    pub fn rubric_version(&self) -> &str {
        &self.rubric_version
    }

    /// Its events, in order.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The head its GitHub snapshot was taken of.
    #[must_use]
    pub const fn head(&self) -> &CommitId {
        self.recorded.head()
    }

    /// The decision its inputs give under `rubric`, when it is the recorded one in canonical
    /// encoding; otherwise where the two first differ.
    pub(crate) fn recompute(
        &self,
        rubric: &Rubric,
    ) -> Result<Result<Decision, DecisionDiff>, serde_json::Error> {
        replay::recompute_recorded(&self.inputs, &self.decision, rubric)
    }
}

/// `assay.run-decision/1` on the wire.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionBundleIn {
    #[serde(rename = "schema")]
    _schema: DecisionSchema,
    run_uid: RunUid,
    evidence: PointerIn,
    trigger: TriggerIn,
    inputs: DecisionInputs,
    rubric_version: String,
    decision: Value,
    events: Vec<Event>,
    github: Value,
}

/// The only decision bundle version this build reads.
#[derive(Deserialize)]
enum DecisionSchema {
    #[serde(rename = "assay.run-decision/1")]
    V1,
}

/// `evidence`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointerIn {
    run_id: NonZeroU64,
    run_attempt: NonZeroU64,
    sha256: String,
}

/// `trigger`.
#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
enum TriggerIn {
    Comment {
        comment_id: NonZeroU64,
        actor_id: NonZeroU64,
    },
    Label {
        actor_id: NonZeroU64,
    },
    Schedule {},
}

/// Reads `bytes` as an `assay.run-decision/1` bundle, as decide-only writes it.
///
/// # Errors
///
/// [`DecisionBundleError::TooLarge`] past [`MAX_DECISION_BUNDLE_BYTES`], unparsed;
/// [`DecisionBundleError::Unreadable`] for text that is not JSON, repeats a key, or is not a
/// bundle the writer writes.
pub fn read_decision_bundle(bytes: &[u8]) -> Result<DecisionBundle, DecisionBundleError> {
    if bytes.len() > MAX_DECISION_BUNDLE_BYTES {
        return Err(DecisionBundleError::TooLarge);
    }
    let value = replay::read_value(bytes).map_err(DecisionBundleError::Unreadable)?;
    let wire: DecisionBundleIn = serde_json::from_value(value)
        .map_err(|_not_run_decision| DecisionBundleError::Unreadable(BundleShape::Shape))?;
    wire.checked()
        .ok_or(DecisionBundleError::Unreadable(BundleShape::Shape))
}

impl DecisionBundleIn {
    /// The bundle, when every part of it is one the writer writes.
    fn checked(self) -> Option<DecisionBundle> {
        let first = NonZeroU64::MIN;
        if self.run_uid.run_attempt != first || self.evidence.run_attempt != first {
            return None;
        }
        let evidence = EvidencePointer {
            run_id: self.evidence.run_id,
            run_attempt: self.evidence.run_attempt,
            sha256: self.evidence.sha256.parse().ok()?,
        };
        let decided_under = self.decision.get("rubric_version").and_then(Value::as_str);
        if self.rubric_version.is_empty()
            || decided_under != Some(self.rubric_version.as_str())
            || self.inputs.decision_key.parse::<Sha256Digest>().is_err()
        {
            return None;
        }
        let trigger = match self.trigger {
            TriggerIn::Comment {
                comment_id,
                actor_id,
            } => DecisionTrigger::Comment {
                comment_id: CommentId(comment_id),
                actor_id: UserId(actor_id),
            },
            TriggerIn::Label { actor_id } => DecisionTrigger::Label {
                actor_id: UserId(actor_id),
            },
            TriggerIn::Schedule {} => DecisionTrigger::Schedule,
        };
        let recorded = RecordedState::read_narrowed(&self.github)?;
        let void = void_records(&self.github)?;
        let bundle = DecisionBundle {
            run_uid: self.run_uid,
            evidence,
            trigger,
            inputs: self.inputs,
            rubric_version: self.rubric_version,
            decision: self.decision,
            events: self.events,
            recorded,
        };
        (written_events(&bundle) && written_contents(&bundle, &void)).then_some(bundle)
    }
}

/// The comment ids of `github.commands` whose status is `void`, in record order; `None` when
/// a record has no such id (the snapshot reader has already refused any other shape).
fn void_records(github: &Value) -> Option<Vec<u64>> {
    github
        .get("commands")?
        .as_array()?
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("void"))
        .map(|record| record.get("comment_id").and_then(Value::as_u64))
        .collect()
}

/// Whether `bundle`'s events say what its snapshot and decision say: one `CommandVoided` per
/// void record (`void`), in order; one `WaiverApplied` per entry of the decision's `waived`,
/// in order; and an `OverrideApplied`, by the inputs' override's issuer, exactly when the
/// decision gives the `override` reason.
fn written_contents(bundle: &DecisionBundle, void: &[u64]) -> bool {
    let payloads = || bundle.events.iter().map(|event| &event.payload);
    let voided: Vec<u64> = payloads()
        .filter_map(|payload| match payload {
            EventPayload::CommandVoided { comment_id, .. } => Some(comment_id.get()),
            _ => None,
        })
        .collect();
    let waivers: Vec<(String, &str)> = payloads()
        .filter_map(|payload| match payload {
            EventPayload::WaiverApplied { who, fp } => Some((fp.to_string(), who.as_str())),
            _ => None,
        })
        .collect();
    let waived: Option<Vec<(String, &str)>> = bundle
        .decision
        .get("waived")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let fp = entry.get("fp")?.as_str()?.to_owned();
                    Some((fp, entry.get("by")?.as_str()?))
                })
                .collect()
        });
    let overridden = bundle
        .decision
        .get("reasons")
        .and_then(Value::as_array)
        .is_some_and(|reasons| {
            reasons
                .iter()
                .any(|reason| reason.get("code").and_then(Value::as_str) == Some("override"))
        });
    let override_by: Vec<&str> = payloads()
        .filter_map(|payload| match payload {
            EventPayload::OverrideApplied { who, .. } => Some(who.as_str()),
            _ => None,
        })
        .collect();
    let expected_by: Vec<&str> = bundle
        .inputs
        .override_record
        .iter()
        .filter(|_| overridden)
        .map(|record| record.by.as_str())
        .collect();
    voided == void && waived.as_ref() == Some(&waivers) && override_by == expected_by
}

/// Whether `bundle`'s events are ones the writer writes: [`EVENT_JOB`]'s, at [`EVENT_STAGE`],
/// numbered from 0, naming the bundle's run and evidence key, all at the first event's time
/// (which reads), and the trigger's command first, then voided records, waivers and at most
/// one override.
fn written_events(bundle: &DecisionBundle) -> bool {
    let events = &bundle.events;
    if events.len() > MAX_DECISION_EVENTS {
        return false;
    }
    let at = events.first().map(|event| event.ts.as_str());
    if at.is_some_and(|ts| ts.parse::<Timestamp>().is_err()) {
        return false;
    }
    let envelopes = (0_u64..).zip(events).all(|(idx, event)| {
        event.idx == idx
            && event.job == EVENT_JOB
            && event.stage == EVENT_STAGE
            && event.run_uid == bundle.run_uid
            && event.run_key == bundle.inputs.evidence_key
            && Some(event.ts.as_str()) == at
    });
    let mut last = Rank::Command;
    let mut overrides = 0_usize;
    for (position, event) in events.iter().enumerate() {
        let Some(rank) = rank(&event.payload, bundle.trigger, position) else {
            return false;
        };
        if rank < last {
            return false;
        }
        if rank == Rank::Override {
            overrides = overrides.saturating_add(1);
        }
        last = rank;
    }
    envelopes && overrides <= 1
}

/// Where a kind of event stands in a bundle's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Command,
    Voided,
    Waiver,
    Override,
}

/// The rank of `payload` at `position`; `None` for a kind a re-decision never records, or a
/// command event that is not first or not the trigger comment's.
fn rank(payload: &EventPayload, trigger: DecisionTrigger, position: usize) -> Option<Rank> {
    let commented = match trigger {
        DecisionTrigger::Comment {
            comment_id,
            actor_id,
        } => Some((comment_id.0, actor_id.0)),
        DecisionTrigger::Label { .. } | DecisionTrigger::Schedule => None,
    };
    match payload {
        EventPayload::CommandRecorded {
            actor_id,
            comment_id,
            ..
        } => {
            (position == 0 && commented == Some((*comment_id, *actor_id))).then_some(Rank::Command)
        }
        EventPayload::CommandRejected { actor_id, .. } => (position == 0
            && commented.is_some_and(|(_, actor)| actor == *actor_id))
        .then_some(Rank::Command),
        EventPayload::CommandVoided { .. } => Some(Rank::Voided),
        EventPayload::WaiverApplied { .. } => Some(Rank::Waiver),
        EventPayload::OverrideApplied { .. } => Some(Rank::Override),
        _ => None,
    }
}
