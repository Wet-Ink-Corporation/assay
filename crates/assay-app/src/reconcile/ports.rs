//! The narrow GitHub port the per-repo reconcile reads and writes one consumer repository
//! through, with the `assay-verdict` App's token (05 §4: `checks: write`, `actions: read`).

use std::num::NonZeroU64;

use crate::config::CommitId;
use crate::forge::CompareAndSetError;
use crate::ledger::LedgerTime;
use crate::publish::{CheckRunId, CheckStatus, CheckUpdate};

/// One consumer repository as its reconcile sees it. Implemented by the forge adapter (#53);
/// every text it returns is untrusted.
pub trait ReconcileForge {
    /// Why GitHub could not be read or written.
    type Error: std::error::Error;

    /// The repository's open PRs, at most `limit` of them: the adapter stops paging once it has
    /// `limit`. The reconcile passes one more than it accepts, so a cut list is refused.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the PRs cannot be listed.
    fn open_pulls(&self, limit: usize) -> Result<Vec<OpenPull>, Self::Error>;

    /// The check runs on `head` that the `assay-verdict` App authored, of every name, at most
    /// `limit` of them. A check of any other app is never returned.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the check runs cannot be listed.
    fn app_checks(&self, head: &CommitId, limit: usize)
    -> Result<Vec<ReconcileCheck>, Self::Error>;

    /// The workflow runs on `head` that have not completed (queued, waiting, pending, requested
    /// or in progress), of every workflow, at most `limit` of them.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the runs cannot be listed.
    fn live_runs(&self, head: &CommitId, limit: usize) -> Result<Vec<LiveRun>, Self::Error>;

    /// The names of the labels on PR `number`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the labels cannot be listed.
    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, Self::Error>;

    /// PR `number` as it is now: open on a head, or closed.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the PR cannot be read.
    fn pull_head(&self, number: NonZeroU64) -> Result<PullHead, Self::Error>;

    /// Overwrites check `id` with `update` only when its `external_id` is still `expected`, as
    /// [`Forge::update_check_if`](crate::Forge::update_check_if): a compare-and-set only for
    /// writers that hold the per-head publish lock.
    ///
    /// # Errors
    ///
    /// [`CompareAndSetError::Conflict`] when the check carries another `external_id`;
    /// [`CompareAndSetError::Forge`] when it cannot be read or written.
    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        update: &CheckUpdate,
    ) -> Result<(), CompareAndSetError<Self::Error>>;
}

/// An open PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPull {
    /// Its number.
    pub number: NonZeroU64,
    /// Its head commit now.
    pub head_sha: CommitId,
}

/// A PR re-read just before a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullHead {
    /// Open, on this head.
    Open(CommitId),
    /// Closed or merged.
    Closed,
}

/// One check run the App authored on a head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileCheck {
    /// Its id.
    pub id: CheckRunId,
    /// Its name.
    pub name: String,
    /// Its `external_id`, the stamp a write compares against; `None` when unset.
    pub external_id: Option<String>,
    /// Its status.
    pub status: CheckStatus,
    /// When GitHub started it (`started_at`), to the second.
    pub started_at: LedgerTime,
}

/// A workflow run on a head that has not completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRun {
    /// Its workflow file, as GitHub names it (`.github/workflows/assay.yml`).
    pub path: String,
}
