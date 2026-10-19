//! Rule (c) of 04 §5 and the push clock: the ledger's push records for the PR (R3-07).
//!
//! `open` appends a record for `synchronize` only, with `at` the triggering workflow run's
//! `created_at`; the `opened` sender is recorded as the opener, not as a pusher. No other event
//! (reopen, ready-for-review, an edit, a dispatch, a re-run or a requeue) records anything, so
//! none of them changes the pusher set or moves the push clock.

use super::timestamp::Timestamp;
use crate::config::CommitId;
use crate::status::UserId;

/// The most push records read for one PR (H-13); more is refused.
pub const MAX_PUSH_RECORDS: usize = 1024;

/// What kind of event a push record records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PushKind {
    /// `opened`: the head the PR arrived with. Its sender is not a pusher.
    Opener,
    /// `synchronize`: a push to the PR's head branch.
    Push,
}

/// One record of `pushes/<repo_id>/<pr>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRecord {
    /// Opener or push.
    pub kind: PushKind,
    /// The head before the push; `None` for the opener.
    pub before: Option<CommitId>,
    /// The head after it.
    pub after: CommitId,
    /// Who sent the event.
    pub sender: UserId,
    /// Whether the push was forced.
    pub forced: bool,
    /// The triggering workflow run's `created_at`.
    pub at: Timestamp,
}

/// The event a workflow run of `open` was triggered by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushTrigger {
    /// `pull_request.opened`.
    Opened {
        /// The head the PR opened with.
        head: CommitId,
        /// The event's sender.
        sender: UserId,
        /// The run's `created_at`.
        at: Timestamp,
    },
    /// `pull_request.synchronize`.
    Synchronize {
        /// The head before the push.
        before: CommitId,
        /// The head after it.
        after: CommitId,
        /// The event's sender.
        sender: UserId,
        /// Whether the push was forced.
        forced: bool,
        /// The run's `created_at`.
        at: Timestamp,
    },
    /// `pull_request.reopened`.
    Reopened,
    /// `pull_request.ready_for_review`.
    ReadyForReview,
    /// `pull_request.edited` (title or body).
    Edited,
    /// `pull_request.edited` with a base change.
    BaseEdited,
    /// `workflow_dispatch`.
    Dispatch,
    /// A re-run of an earlier workflow run.
    Rerun,
    /// A requeue of a deferred run (rule 8a).
    Requeue,
}

/// The push record `open` appends for `trigger`: an opener for `opened`, a push for
/// `synchronize`, nothing for any other event.
#[must_use]
pub fn push_record(trigger: PushTrigger) -> Option<PushRecord> {
    match trigger {
        PushTrigger::Opened { head, sender, at } => Some(PushRecord {
            kind: PushKind::Opener,
            before: None,
            after: head,
            sender,
            forced: false,
            at,
        }),
        PushTrigger::Synchronize {
            before,
            after,
            sender,
            forced,
            at,
        } => Some(PushRecord {
            kind: PushKind::Push,
            before: Some(before),
            after,
            sender,
            forced,
            at,
        }),
        PushTrigger::Reopened
        | PushTrigger::ReadyForReview
        | PushTrigger::Edited
        | PushTrigger::BaseEdited
        | PushTrigger::Dispatch
        | PushTrigger::Rerun
        | PushTrigger::Requeue => None,
    }
}

/// The push clock of `head`: the latest `at` of a record whose `after` is `head` (the opener's
/// for a head that arrived with `opened`); `None` when no record names it.
pub(crate) fn push_clock(pushes: &[PushRecord], head: &CommitId) -> Option<Timestamp> {
    pushes
        .iter()
        .filter(|record| record.after == *head)
        .map(|record| record.at)
        .max()
}
