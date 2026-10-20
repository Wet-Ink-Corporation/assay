//! What the snapshot reads besides `Forge` and the git range: the ledger's push and command
//! records (written by `open` and decide-only, #70's adapter) and the comments behind commands.

use std::num::NonZeroU64;

use super::commands::{IssueComment, SnapshotCommand};
use crate::ledger::PushRecord;
use crate::publish::CommentId;

/// The ledger's per-PR records the eligibility rule reads (02 §5): `pushes/<repo_id>/<pr>.jsonl`
/// and `commands/<repo_id>/<pr>.jsonl` of the repository the adapter was built for.
pub trait SnapshotLedger {
    /// Why the ledger could not be read.
    type Error: std::error::Error;

    /// Every push record of PR `number`, in append order, or [`LedgerRecords::TooMany`] when
    /// there are more than `max`. An adapter reads at most `max + 1` records to tell (H-13).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the records cannot be read or one does not parse.
    fn push_records(
        &self,
        number: NonZeroU64,
        max: usize,
    ) -> Result<LedgerRecords<PushRecord>, Self::Error>;

    /// Every command record of PR `number`, in append order, or [`LedgerRecords::TooMany`] when
    /// there are more than `max`. An adapter reads at most `max + 1` records to tell (H-13).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the records cannot be read or one does not parse.
    fn command_records(
        &self,
        number: NonZeroU64,
        max: usize,
    ) -> Result<LedgerRecords<SnapshotCommand>, Self::Error>;
}

/// What [`SnapshotLedger`] listed for a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerRecords<T> {
    /// Every record, in append order.
    Listed(Vec<T>),
    /// More records than the cap.
    TooMany,
}

impl<T> LedgerRecords<T> {
    /// The records when there are at most `max` of them; an adapter that listed more anyway
    /// is read as having said so.
    pub(crate) fn within(self, max: usize) -> Option<Vec<T>> {
        match self {
            Self::Listed(records) if records.len() <= max => Some(records),
            Self::Listed(_) | Self::TooMany => None,
        }
    }
}

/// The longest comment body a [`CommandComments`] adapter reads, in bytes: GitHub caps a body at
/// 65,536 characters, at most four bytes each.
pub const MAX_COMMENT_BYTES: usize = 262_144;

/// Issue comments re-fetched to check command records (`GET /issues/comments/{id}`).
///
/// Read by the eligibility snapshot and by `admit`'s dismissals (05 T21, R3-02). The record,
/// never the comment, is what a command says; the comment only shows whether it still stands.
/// Everything read is untrusted.
pub trait CommandComments {
    /// Why GitHub could not be read.
    type Error: std::error::Error;

    /// Comment `id` as it stands now; `None` when it was deleted.
    ///
    /// An implementation must refuse a body over [`MAX_COMMENT_BYTES`].
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when GitHub cannot be read, or the comment's body is over the cap.
    fn comment(&self, id: CommentId) -> Result<Option<IssueComment>, Self::Error>;
}
