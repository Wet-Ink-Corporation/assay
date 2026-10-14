//! The one narrow read port `status` reads GitHub through, and what it returns. The run's
//! artifacts are read through [`BundleSource`](crate::BundleSource), as replay reads them.

use std::num::NonZeroU64;

use crate::config::CommitId;
use crate::publish::CheckStatus;

/// What `assay status` reads from GitHub with the viewer's own `gh` token. Implemented by the
/// forge adapter (#53). Every text it returns is untrusted: `status` validates it.
pub trait StatusForge {
    /// Why GitHub could not be read.
    type Error: std::error::Error;

    /// PR `number`, open or not; `None` when the repository has no such PR.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the PR cannot be read.
    fn pull(&self, number: NonZeroU64) -> Result<Option<PullView>, Self::Error>;

    /// The open PRs of the repository whose head is `branch` of the repository itself, at
    /// most `limit` of them: the adapter stops paging once it has `limit`. `status` passes one
    /// more than it accepts, so a list cut at `limit` is refused, never read as complete.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the PRs cannot be listed.
    fn open_pulls(&self, branch: &str, limit: usize) -> Result<Vec<PullView>, Self::Error>;

    /// The check runs on `head`, of any app, across pages up to `limit` of them, each with the
    /// id of the app that created it. `status` keeps only the configured App's, and passes one
    /// more than its cap so that a cut list is refused.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the check runs cannot be listed.
    fn check_runs(&self, head: &CommitId, limit: usize) -> Result<Vec<CheckView>, Self::Error>;

    /// The review comments on PR `number` authored by App `app`, across pages up to `limit` of
    /// them. A comment by anyone else is never returned; `status` checks each one's
    /// [`ReviewCommentView::app`] again, and passes one more than its cap so that a cut list is
    /// refused.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the review comments cannot be listed.
    fn review_comments(
        &self,
        number: NonZeroU64,
        app: AppId,
        limit: usize,
    ) -> Result<Vec<ReviewCommentView>, Self::Error>;

    /// The authenticated `gh` user running the command.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the user cannot be read.
    fn viewer(&self) -> Result<Viewer, Self::Error>;
}

/// A GitHub App's numeric id (`check_run.app.id`, the ruleset's `integration_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppId(pub NonZeroU64);

/// A GitHub user's numeric id, the authoritative identity of 03 §1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub NonZeroU64);

/// A PR as GitHub shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullView {
    /// Its number.
    pub number: NonZeroU64,
    /// Its head commit.
    pub head_sha: CommitId,
    /// Its head branch.
    pub head_ref: String,
    /// Its base branch, which names the verdict check (R3-03).
    pub base_ref: String,
}

/// One check run on a commit, of any app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckView {
    /// The app that created it.
    pub app: AppId,
    /// Its name.
    pub name: String,
    /// Its status.
    pub status: CheckStatus,
    /// Its conclusion, once completed.
    pub conclusion: Option<ObservedConclusion>,
    /// Its `external_id`, if set.
    pub external_id: Option<String>,
    /// Its `output.text`, if set.
    pub output_text: Option<String>,
    /// Its page (`html_url`).
    pub url: String,
}

/// A check run's conclusion, as GitHub can report it for any app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObservedConclusion {
    /// `success`.
    Success,
    /// `failure`.
    Failure,
    /// `neutral`.
    Neutral,
    /// `cancelled`.
    Cancelled,
    /// `skipped`.
    Skipped,
    /// `timed_out`.
    TimedOut,
    /// `action_required`.
    ActionRequired,
    /// `stale`: GitHub closed a check left incomplete for 14 days.
    Stale,
}

impl ObservedConclusion {
    /// The conclusion's wire word.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Neutral => "neutral",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
            Self::ActionRequired => "action_required",
            Self::Stale => "stale",
        }
    }
}

/// One review comment on a PR's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCommentView {
    /// The app that authored it; `None` for a user.
    pub app: Option<AppId>,
    /// Its body as GitHub returns it.
    pub body: String,
    /// The file it is on.
    pub path: String,
    /// Its line (GitHub's `line`); `None` once the line is outdated.
    pub line: Option<NonZeroU64>,
    /// Its page (`html_url`).
    pub url: String,
}

/// The user `gh` is authenticated as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// Their numeric id.
    pub id: UserId,
    /// Their login, for display.
    pub login: String,
}
