//! The hourly poll's selection (D8 A): which open PRs a decide-only run decides again without a
//! command, because an approval arrived after the verdict asked for one.

use std::num::NonZeroU64;

use assay_domain::ReasonCode;

use super::{MAX_POLL_PULLS, MAX_POLL_SELECTED, PollForge, VerdictView};
use crate::publish::read_refusal_title;
use crate::{CheckName, CommitId, ObservedConclusion, Review, ReviewState, Timestamp};

/// Why the poll could not select.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PollError<E> {
    /// The open PRs could not be listed.
    #[error("listing the open PRs failed")]
    Pulls(#[source] E),
    /// A PR's verdict check could not be read.
    #[error("reading PR #{pr}'s verdict check failed")]
    Check {
        /// The PR.
        pr: NonZeroU64,
        /// The read's failure.
        #[source]
        source: E,
    },
    /// A PR's reviews could not be read.
    #[error("reading PR #{pr}'s reviews failed")]
    Reviews {
        /// The PR.
        pr: NonZeroU64,
        /// The read's failure.
        #[source]
        source: E,
    },
}

/// The open PRs (at most [`MAX_POLL_PULLS`]) waiting on an approval that has since arrived, at
/// most [`MAX_POLL_SELECTED`], the one whose verdict check completed first first (then by PR
/// number).
///
/// A PR waits when the `assay-verdict` App's check on its head, named for its base in a
/// repository whose default branch is `default_branch`, completed as `failure` with the title
/// `assay status` reads as exactly `trust-path-unapproved` (#218's reader, `assay: failure
/// (trust-path-unapproved)`). The approval is an
/// `APPROVED` review of that head submitted strictly after the check completed; whether it
/// counts is the re-decision's to judge. Reviews are read only for a PR that waits.
///
/// # Errors
///
/// [`PollError`] for the first read that fails, naming the PR it was for.
pub fn poll_pending<P: PollForge>(
    forge: &P,
    default_branch: &str,
) -> Result<Vec<NonZeroU64>, PollError<P::Error>> {
    let mut pending: Vec<(Timestamp, NonZeroU64)> = Vec::new();
    for pull in forge.open_pulls(MAX_POLL_PULLS).map_err(PollError::Pulls)? {
        let pr = pull.number;
        let name = CheckName::for_base(&pull.base_ref, default_branch);
        let check = forge
            .verdict_check(&pull.head_sha, &name)
            .map_err(|source| PollError::Check { pr, source })?;
        let Some(VerdictView::Completed(check)) = check else {
            continue;
        };
        let unapproved = check.conclusion == ObservedConclusion::Failure
            && check.title.as_deref().and_then(read_refusal_title)
                == Some(ReasonCode::TrustPathUnapproved);
        if !unapproved {
            continue;
        }
        let reviews = forge
            .reviews(pr)
            .map_err(|source| PollError::Reviews { pr, source })?;
        let approved_since = reviews.iter().any(|review| {
            approves(review, &pull.head_sha)
                && review
                    .submitted_at
                    .is_some_and(|submitted| submitted > check.completed_at)
        });
        if approved_since {
            pending.push((check.completed_at, pr));
        }
    }
    pending.sort_unstable();
    Ok(pending
        .into_iter()
        .take(MAX_POLL_SELECTED)
        .map(|(_, pr)| pr)
        .collect())
}

/// Whether `review` approves `head`.
fn approves(review: &Review, head: &CommitId) -> bool {
    review.state == ReviewState::Approved && review.commit.as_ref() == Some(head)
}
