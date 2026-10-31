//! The narrow GitHub port the per-repo reconcile reads and writes one consumer repository
//! through, with the `assay-verdict` App's token (05 §4: `checks: write`, `actions: read`).

use std::num::NonZeroU64;

use crate::config::CommitId;
use crate::forge::{CompareAndSetError, LockedWrite};
use crate::ledger::LedgerTime;
use crate::publish::{CheckRunId, CheckStatus};

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

    /// The repository's workflow run `run_id`, whatever commit it runs on (a requeue's
    /// `workflow_dispatch` and decide-only's runs are on the default branch's); `None` when
    /// GitHub has no such run.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the run cannot be read.
    fn workflow_run(&self, run_id: NonZeroU64) -> Result<Option<StampedRun>, Self::Error>;

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

    /// Overwrites check `id` with `write.update` only when its `external_id` is still
    /// `expected`, as [`Forge::update_check_if`](crate::Forge::update_check_if): a
    /// compare-and-set only for writers that hold the per-head publish lock, so nothing is sent,
    /// and no retry waited for, unless it ends before `write.deadline`, the lock lease's
    /// `expires_at` (#290).
    ///
    /// # Errors
    ///
    /// [`CompareAndSetError::Conflict`] when the check carries another `external_id`;
    /// [`CompareAndSetError::Forge`] when it cannot be read or written, or the next request or
    /// wait would not end before the deadline.
    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        write: LockedWrite<'_>,
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
}

/// The workflow run a verdict check's `pending:<run_id>:<seq>` stamp names: the last run that
/// opened the check, so the one that would finish it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampedRun {
    /// Whether it is still running.
    pub progress: RunProgress,
    /// When its attempt started (`run_started_at`), to the second.
    pub started_at: LedgerTime,
}

/// Whether a workflow run has completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunProgress {
    /// Queued, waiting, pending, requested or in progress: it may still publish.
    Live,
    /// Completed, whatever its conclusion.
    Completed,
}
