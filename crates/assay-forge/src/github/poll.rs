//! `assay-app`'s `PollForge` port (#61 DO-9, D8): the hourly decide-only poll's reads, with a
//! read-only token. The open PRs share the reconcile's listing and the verdict check the
//! publication's lookup by app id, so each rule exists once.

use std::num::NonZeroU64;

use assay_app::{
    CheckName, CheckStatus, CommitId, CompletedVerdict, PollForge, PolledPull, Review, Timestamp,
    VerdictView,
};

use super::GitHub;
use super::checks::check_status;
use super::dto::{self, CheckRunDto};
use super::error::{ForgeError, Operation};
use super::status::observed_conclusion;
use super::transport::{Sleep, Transport};

impl<T: Transport, S: Sleep> PollForge for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn open_pulls(&self, limit: usize) -> Result<Vec<PolledPull>, Self::Error> {
        let operation = Operation::ListPulls;
        self.open_pull_dtos(limit)?
            .into_iter()
            .map(|pull| {
                Ok(PolledPull {
                    number: dto::nonzero(operation, pull.number, "a PR number is zero")?,
                    head_sha: dto::commit(operation, &pull.head.sha)?,
                    base_ref: pull.base.branch,
                })
            })
            .collect()
    }

    /// The poll reads outside any lock, so with no deadline.
    fn verdict_check(
        &self,
        head: &CommitId,
        name: &CheckName,
    ) -> Result<Option<VerdictView>, Self::Error> {
        self.app_check(head, name, None)?
            .map(verdict_view)
            .transpose()
    }

    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, Self::Error> {
        self.list_reviews(number)
    }
}

/// The App's verdict `check` as the poll reads it: a completed one must carry its conclusion
/// and completion time.
fn verdict_view<E>(check: CheckRunDto) -> Result<VerdictView, ForgeError<E>> {
    let operation = Operation::ListChecks;
    let malformed = |detail| ForgeError::Malformed { operation, detail };
    match check_status(operation, &check.status)? {
        CheckStatus::Queued | CheckStatus::InProgress => Ok(VerdictView::Pending),
        CheckStatus::Completed => {
            let conclusion = check
                .conclusion
                .as_deref()
                .ok_or_else(|| malformed("a completed check has no conclusion"))?;
            let completed_at = check
                .completed_at
                .as_deref()
                .ok_or_else(|| malformed("a completed check has no completed_at"))?
                .parse::<Timestamp>()
                .map_err(|_not_utc| malformed("a check's completed_at is not a UTC date-time"))?;
            Ok(VerdictView::Completed(CompletedVerdict {
                conclusion: observed_conclusion(operation, conclusion)?,
                title: check.output.title,
                completed_at,
            }))
        }
    }
}
