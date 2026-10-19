//! What the snapshot reads besides `Forge` and the git range: the ledger's push and command
//! records (written by `open` and decide-only, #70's adapter) and the comments behind commands.

use std::num::NonZeroU64;

use super::commands::{CommandRecord, IssueComment};
use super::pushes::PushRecord;
use crate::publish::CommentId;

/// The ledger's per-PR records the eligibility rule reads (02 §5): `pushes/<repo_id>/<pr>.jsonl`
/// and `commands/<repo_id>/<pr>.jsonl` of the repository the adapter was built for.
pub trait SnapshotLedger {
    /// Why the ledger could not be read.
    type Error: std::error::Error;

    /// Every push record of PR `number`, in append order.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the records cannot be read or one does not parse.
    fn push_records(&self, number: NonZeroU64) -> Result<Vec<PushRecord>, Self::Error>;

    /// Every command record of PR `number`, in append order.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the records cannot be read or one does not parse.
    fn command_records(&self, number: NonZeroU64) -> Result<Vec<CommandRecord>, Self::Error>;
}

/// Issue comments re-fetched to check command records (`GET /issues/comments/{id}`).
pub trait CommandComments {
    /// Why GitHub could not be read.
    type Error: std::error::Error;

    /// Comment `id` as it stands now; `None` when it was deleted.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when GitHub cannot be read.
    fn comment(&self, id: CommentId) -> Result<Option<IssueComment>, Self::Error>;
}
