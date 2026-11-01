//! decide-only's decision bundle (`assay.run-decision/1`, 03 §6e, D5, J10): what one
//! re-decision decided and from what, chained to the evidence bundle by its pointer so replay
//! can follow the decision to its evidence.

use std::num::NonZeroU64;

use assay_domain::{Decision, DecisionInputs, Event, EventPayload, EventSchema, Rubric, RunUid};
use serde::Serialize;
use thiserror::Error;

use super::read::{DecisionBundleError, read_decision_bundle};
use super::{EvidenceBundle, MAX_DECISION_BUNDLE_BYTES};
use crate::config::{CommitId, Sha256Digest};
use crate::eligibility::{
    CommandRejected, GitHubState, IssueComment, SnapshotCommand, StateRecord, Timestamp,
    waiver_events,
};
use crate::publish::{CappedJson, CommentId, PreparedBundle, capped_json};
use crate::replay::DecisionDiff;
use crate::status::UserId;

/// The job decide-only's re-decision events belong to: the workflow's `redecide` job.
pub(super) const EVENT_JOB: &str = "redecide";

/// The pipeline stage of a re-decision's events: Decide (04 §1).
pub(super) const EVENT_STAGE: &str = "S7";

/// The most events a decision bundle holds: the trigger's command, one per command record
/// voided or waiver applied (each at most [`MAX_COMMAND_RECORDS`](crate::MAX_COMMAND_RECORDS)),
/// and an override, well within this.
pub(super) const MAX_DECISION_EVENTS: usize = 1024;

/// What started a re-decision (03 §6e `trigger`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionTrigger {
    /// An `issue_comment` `created` event: the comment and its author.
    Comment {
        /// The triggering comment.
        comment_id: CommentId,
        /// Its author.
        actor_id: UserId,
    },
    /// An `assay:*` label change, and who made it.
    Label {
        /// The event's sender.
        actor_id: UserId,
    },
    /// The hourly poll.
    Schedule,
}

/// What became of the triggering comment's command, for its event.
#[derive(Debug, Clone, Copy)]
pub enum TriggerCommand<'a> {
    /// It was recorded ([`Recorded::Record`](crate::Recorded::Record)): `CommandRecorded`.
    Recorded(&'a SnapshotCommand),
    /// It was rejected: `CommandRejected`, with the grammar's word and the closed reason.
    Rejected {
        /// Why.
        reason: CommandRejected,
        /// The triggering comment.
        comment: &'a IssueComment,
    },
}

/// The decide-only run whose bundle is prepared, and the PR as it is now.
#[derive(Debug, Clone, Copy)]
pub struct DecisionTarget<'a> {
    /// decide-only's run (attempt 1: no other writes).
    pub run_uid: RunUid,
    /// The PR's live head.
    pub head_sha: &'a CommitId,
    /// The PR's live base ref.
    pub base_ref: &'a str,
    /// What started the run.
    pub trigger: DecisionTrigger,
    /// When it decided: every event's `ts`.
    pub at: Timestamp,
}

/// One re-decision and what it was decided from.
#[derive(Debug, Clone, Copy)]
pub struct Redecided<'a> {
    /// The authenticated evidence bundle of the head.
    pub evidence: &'a EvidenceBundle,
    /// This build's rubric.
    pub rubric: &'a Rubric,
    /// The inputs [`redecision_inputs`](crate::redecision_inputs) composed.
    pub inputs: &'a DecisionInputs,
    /// The snapshot `inputs` were built from, recorded in the bundle.
    pub github: &'a GitHubState,
    /// What became of the triggering comment's command; `None` for a recheck, a label or the
    /// poll.
    pub command: Option<TriggerCommand<'a>>,
}

/// Why [`prepare_decision`] built no bundle. Nothing is persisted or published.
#[derive(Debug, Error)]
pub enum PrepareDecisionError {
    /// The run is a re-run: only attempt 1 writes (R3-05).
    #[error("only attempt 1 persists a decision bundle")]
    NotFirstAttempt,
    /// The decision key is not `sha256:<64 hex>`, so it could be mistaken for a stamp.
    #[error("the decision key is not a sha256 digest")]
    DecisionKey,
    /// The inputs were built over another evidence key than the evidence bundle's.
    #[error("the decision inputs' evidence key is not the evidence bundle's")]
    EvidenceKey,
    /// The GitHub state snapshot was taken of another head than the PR's.
    #[error("the GitHub state snapshot is not of the PR's head")]
    SnapshotHead,
    /// The evidence bundle decided another head than the PR's.
    #[error("the evidence bundle is not of the PR's head")]
    EvidenceHead,
    /// The inputs' or the evidence's base ref is not the PR's.
    #[error("the decision's base ref is not the PR's")]
    BaseRef,
    /// The inputs' classes are not the evidence's, so the bundle would not say which PRs fail
    /// closed (a re-decision never reclassifies).
    #[error("the decision inputs' classes are not the evidence's")]
    Classes,
    /// The inputs' tier is not the evidence's, the tier its LLM stage ran at.
    #[error("the decision inputs' tier is not the evidence's")]
    Tier,
    /// The evidence was decided under another rubric version than the one given.
    #[error("the evidence was decided under another rubric version")]
    RubricVersion,
    /// The command is not the triggering comment's, on the PR's head, or its rejection names
    /// no command and author.
    #[error("the command is not the triggering comment's")]
    Trigger,
    /// The bundle would be larger than [`MAX_DECISION_BUNDLE_BYTES`].
    #[error("the decision bundle is larger than {MAX_DECISION_BUNDLE_BYTES} bytes")]
    TooLarge,
    /// The bundle could not be encoded: a defect in decide-only.
    #[error("could not encode the decision bundle")]
    Encode(#[source] serde_json::Error),
    /// The bundle built does not read back as what it was built from: a defect in decide-only.
    #[error("the decision bundle does not read back")]
    ReadBack(#[source] DecisionReadBack),
}

/// How a built decision bundle failed to read back.
#[derive(Debug, Error)]
pub enum DecisionReadBack {
    /// The reader refuses it.
    #[error(transparent)]
    Unreadable(DecisionBundleError),
    /// It reads, but records another run, evidence, trigger or head than it was built for.
    #[error("it records another run, evidence, trigger or head")]
    Fields,
    /// Its recorded decision is not the one its inputs give.
    #[error("its recorded decision is not the one its inputs give")]
    Decision(DecisionDiff),
    /// A decision could not be encoded.
    #[error("could not encode a decision")]
    Encode(#[source] serde_json::Error),
}

/// `assay.run-decision/1`.
#[derive(Serialize)]
struct RunDecision<'a> {
    schema: &'static str,
    run_uid: RunUid,
    evidence: PointerOut<'a>,
    trigger: TriggerOut,
    inputs: &'a DecisionInputs,
    rubric_version: &'a str,
    decision: &'a Decision,
    events: &'a [Event],
    github: StateRecord<'a>,
}

/// `evidence`.
#[derive(Serialize)]
struct PointerOut<'a> {
    run_id: NonZeroU64,
    run_attempt: NonZeroU64,
    sha256: &'a str,
}

/// `trigger`.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
enum TriggerOut {
    Comment {
        comment_id: NonZeroU64,
        actor_id: NonZeroU64,
    },
    Label {
        actor_id: NonZeroU64,
    },
    Schedule,
}

impl From<DecisionTrigger> for TriggerOut {
    fn from(trigger: DecisionTrigger) -> Self {
        match trigger {
            DecisionTrigger::Comment {
                comment_id,
                actor_id,
            } => Self::Comment {
                comment_id: comment_id.0,
                actor_id: actor_id.0,
            },
            DecisionTrigger::Label { actor_id } => Self::Label {
                actor_id: actor_id.0,
            },
            DecisionTrigger::Schedule => Self::Schedule,
        }
    }
}

/// Builds `redecided`'s `assay.run-decision/1` bundle for `target` and its sha256, after the
/// checks [`prepare`](crate::prepare) makes, and reads it back.
///
/// The bundle records decide-only's run, the evidence pointer, the trigger, the inputs, the
/// rubric version, the decision `redecided.inputs` give, its events and the snapshot. The events
/// are, in order: the triggering comment's `CommandRecorded` or `CommandRejected`, one
/// `CommandVoided` per void command record of the snapshot, one `WaiverApplied` per waiver the
/// decision applied, and `OverrideApplied` when it applied an override; each of job `redecide`
/// at stage `S7`, stamped `target.at`.
///
/// # Errors
///
/// [`PrepareDecisionError`] when the target is a re-run, the inputs, snapshot, evidence or
/// command do not fit it, or the bundle cannot be built or read back.
pub fn prepare_decision(
    target: &DecisionTarget<'_>,
    redecided: &Redecided<'_>,
) -> Result<PreparedBundle, PrepareDecisionError> {
    fits(target, redecided)?;
    let command = command_event(target, redecided.command)?;
    let decision = assay_domain::decide(redecided.rubric, redecided.inputs);
    let events = events(target, redecided, command, &decision);
    let bundle = RunDecision {
        schema: "assay.run-decision/1",
        run_uid: target.run_uid,
        evidence: PointerOut {
            run_id: redecided.evidence.pointer().run_id,
            run_attempt: redecided.evidence.pointer().run_attempt,
            sha256: redecided.evidence.pointer().sha256.as_str(),
        },
        trigger: target.trigger.into(),
        inputs: redecided.inputs,
        rubric_version: &decision.rubric_version,
        decision: &decision,
        events: &events,
        github: StateRecord::of(redecided.github),
    };
    let bytes = capped_json(&bundle, MAX_DECISION_BUNDLE_BYTES).map_err(|error| match error {
        CappedJson::TooLarge => PrepareDecisionError::TooLarge,
        CappedJson::Encode(error) => PrepareDecisionError::Encode(error),
    })?;
    read_back(target, redecided, &bytes).map_err(PrepareDecisionError::ReadBack)?;
    Ok(PreparedBundle::of(bytes))
}

/// The checks that the inputs, snapshot and evidence are of `target`'s run and PR.
fn fits(
    target: &DecisionTarget<'_>,
    redecided: &Redecided<'_>,
) -> Result<(), PrepareDecisionError> {
    let (inputs, evidence) = (redecided.inputs, redecided.evidence);
    if target.run_uid.run_attempt != NonZeroU64::MIN {
        return Err(PrepareDecisionError::NotFirstAttempt);
    }
    if inputs.decision_key.parse::<Sha256Digest>().is_err() {
        return Err(PrepareDecisionError::DecisionKey);
    }
    if inputs.evidence_key != evidence.inputs().evidence_key {
        return Err(PrepareDecisionError::EvidenceKey);
    }
    if redecided.github.head() != target.head_sha {
        return Err(PrepareDecisionError::SnapshotHead);
    }
    if evidence.head() != target.head_sha {
        return Err(PrepareDecisionError::EvidenceHead);
    }
    if inputs.pr.base_ref != target.base_ref || evidence.base_ref() != target.base_ref {
        return Err(PrepareDecisionError::BaseRef);
    }
    let recorded = &evidence.inputs().pr;
    if inputs.pr.classes != recorded.classes {
        return Err(PrepareDecisionError::Classes);
    }
    if inputs.pr.tier != recorded.tier {
        return Err(PrepareDecisionError::Tier);
    }
    if evidence.decision().rubric_version != redecided.rubric.version() {
        return Err(PrepareDecisionError::RubricVersion);
    }
    Ok(())
}

/// The triggering comment's command event, when it fits the trigger.
fn command_event(
    target: &DecisionTarget<'_>,
    command: Option<TriggerCommand<'_>>,
) -> Result<Option<EventPayload>, PrepareDecisionError> {
    let Some(command) = command else {
        return Ok(None);
    };
    let DecisionTrigger::Comment {
        comment_id,
        actor_id,
    } = target.trigger
    else {
        return Err(PrepareDecisionError::Trigger);
    };
    match command {
        TriggerCommand::Recorded(record)
            if record.comment_id == comment_id
                && record.actor == actor_id
                && &record.head_sha == target.head_sha =>
        {
            Ok(Some(EventPayload::CommandRecorded {
                // The event names the grammar's word, never the comment's text.
                command: record.command.name().to_owned(),
                actor_id: actor_id.0,
                comment_id: comment_id.0,
            }))
        }
        TriggerCommand::Rejected { reason, comment }
            if comment.id == comment_id && comment.user == Some(actor_id) =>
        {
            reason
                .event(comment)
                .map(Some)
                .ok_or(PrepareDecisionError::Trigger)
        }
        TriggerCommand::Recorded(_) | TriggerCommand::Rejected { .. } => {
            Err(PrepareDecisionError::Trigger)
        }
    }
}

/// The re-decision's events, in the bundle's order, each with `target`'s envelope.
fn events(
    target: &DecisionTarget<'_>,
    redecided: &Redecided<'_>,
    command: Option<EventPayload>,
    decision: &Decision,
) -> Vec<Event> {
    let github = redecided.github;
    let voided = github
        .voided()
        .map(|(record, cause)| EventPayload::CommandVoided {
            comment_id: record.comment_id.0,
            reason: cause.event_reason(),
        });
    let payloads = command
        .into_iter()
        .chain(voided)
        .chain(waiver_events(decision))
        .chain(github.override_applied(decision));
    let ts = target.at.to_string();
    (0_u64..)
        .zip(payloads)
        .map(|(idx, payload)| Event {
            schema: EventSchema::V1,
            job: EVENT_JOB.to_owned(),
            idx,
            // Each event owns its copies of the time and the evidence key.
            ts: ts.clone(),
            run_key: redecided.inputs.evidence_key.clone(),
            run_uid: target.run_uid,
            stage: EVENT_STAGE.to_owned(),
            payload,
        })
        .collect()
}

/// Reads `bytes` back as `redecided`'s bundle for `target`.
fn read_back(
    target: &DecisionTarget<'_>,
    redecided: &Redecided<'_>,
    bytes: &[u8],
) -> Result<(), DecisionReadBack> {
    let back = read_decision_bundle(bytes).map_err(DecisionReadBack::Unreadable)?;
    if back.run_uid() != target.run_uid
        || back.evidence() != redecided.evidence.pointer()
        || back.trigger() != target.trigger
        || back.head() != target.head_sha
    {
        return Err(DecisionReadBack::Fields);
    }
    back.recompute(redecided.rubric)
        .map_err(DecisionReadBack::Encode)?
        .map_err(DecisionReadBack::Decision)?;
    Ok(())
}
