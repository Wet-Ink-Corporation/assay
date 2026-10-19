//! Rule (c) of 04 §5 and the push clock, read from the ledger's push records for the PR
//! (`assay.ledger-push/1`, R3-07).
//!
//! `open` appends an opener record for `opened` and a push record for each `synchronize`, with
//! `at` the triggering workflow run's `created_at`. No other event (reopen, ready-for-review, an
//! edit, a dispatch, a re-run or a requeue) records anything, so none of them changes the pusher
//! set or moves the push clock. The opener's sender is not a pusher.

use super::timestamp::Timestamp;
use crate::config::CommitId;
use crate::ledger::{PushKind, PushRecord};
use crate::status::UserId;

/// The most push records read for one PR (H-13); more is refused.
pub const MAX_PUSH_RECORDS: usize = 1024;

/// What a push record says for rule (c): the head before a push, the head after it, and who
/// pushed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pushed<'a> {
    /// The head before the push.
    pub(crate) before: &'a CommitId,
    /// The head after it.
    pub(crate) after: &'a CommitId,
    /// Who pushed.
    pub(crate) sender: UserId,
}

/// `record` as a push, or `None` for the opener, whose sender is not a pusher.
pub(crate) fn pushed(record: &PushRecord) -> Option<Pushed<'_>> {
    match &record.push.kind {
        PushKind::Push { before, .. } => Some(Pushed {
            before,
            after: &record.push.after,
            sender: record.push.sender_id,
        }),
        PushKind::Opener => None,
    }
}

/// The push clock of `head`: the latest `at` of a record whose `after` is `head` (the opener's
/// for a head that arrived with `opened`); `None` when no record names it.
pub(crate) fn push_clock(pushes: &[PushRecord], head: &CommitId) -> Option<Timestamp> {
    pushes
        .iter()
        .filter(|record| record.push.after == *head)
        .map(|record| Timestamp::from(record.push.at))
        .max()
}
