//! The `Forge` port: the GitHub calls the narrow use-case ports do not hold (02 §2, §5). `open`
//! finds or creates the verdict check and re-stamps it by compare-and-set; `decide`'s GitHub
//! state snapshot reads reviews and commit identities (#58); feedback reads reactions and the
//! timeline (R-12). Implemented by the forge adapter (#53). The calls made under the publish
//! lock take its lease's [`Deadline`] (#290).

mod deadline;

use std::num::NonZeroU64;

use crate::config::CommitId;
use crate::eligibility::Timestamp;
use crate::publish::{CheckName, CheckRun, CheckRunId, CheckUpdate, CommentId};
use crate::status::UserId;

pub use deadline::Deadline;

/// GitHub as the `assay-verdict` App sees one repository. Every text it returns is untrusted.
pub trait Forge {
    /// Why GitHub could not be read or written.
    type Error: std::error::Error;

    /// The check run named `name` on `head` that the App authored, created with
    /// `initial.update` when there is none. A same-named check of any other app is never
    /// returned, and never stops the App's from being created.
    ///
    /// Made under the per-head publish lock: no request is sent, and no retry waited for, unless
    /// it ends before `initial.deadline`, the lock lease's `expires_at` (#290).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the check runs cannot be listed or the check cannot be created, or
    /// when the next request or wait would not end before the deadline (it is not made).
    fn find_or_create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: LockedWrite<'_>,
    ) -> Result<CheckFound, Self::Error>;

    /// Overwrites check `id` with `write.update` only when its `external_id` is still
    /// `expected`.
    ///
    /// GitHub has no atomic check update (02 §5), so this reads, compares and then writes:
    /// it is a compare-and-set only for writers that hold the per-head publish lock, and so
    /// sends nothing, and waits for no retry, that would not end before `write.deadline`, the
    /// lock lease's `expires_at` (#290).
    ///
    /// # Errors
    ///
    /// [`CompareAndSetError::Conflict`] when the check carries another `external_id`; nothing
    /// was written. [`CompareAndSetError::Forge`] when the check cannot be read or updated,
    /// another app authored it, or the next request or wait would not end before the deadline.
    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        write: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<Self::Error>>;

    /// Makes the App's comment on PR `number` whose body contains `marker` read `body`: posted
    /// when there is none, edited when its body differs, left alone when it is equal.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comments cannot be listed or the comment cannot be written.
    fn upsert_comment(
        &self,
        number: NonZeroU64,
        marker: &str,
        body: &str,
    ) -> Result<Upserted, Self::Error>;

    /// The GitHub users behind each commit's authors (co-authors included) and committer, in
    /// the order of `commits`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when a commit cannot be resolved, including a commit GitHub does not
    /// know.
    fn commit_identities(&self, commits: &[CommitId]) -> Result<Vec<CommitIdentity>, Self::Error>;

    /// Every review on PR `number`, oldest first.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the reviews cannot be listed.
    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, Self::Error>;

    /// The names of the labels on PR `number`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the labels cannot be listed.
    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, Self::Error>;

    /// Every reaction on `subject`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the reactions cannot be listed.
    fn reactions(&self, subject: ReactionSubject) -> Result<Vec<Reaction>, Self::Error>;

    /// The timeline of PR `number`, oldest first.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the timeline cannot be read.
    fn timeline(&self, number: NonZeroU64) -> Result<Vec<TimelineEvent>, Self::Error>;
}

/// A check write made under the per-head publish lock: what to write, and the lock lease's
/// expiry, which the call's every request and retry wait must end before (#290).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockedWrite<'a> {
    /// The check's new state.
    pub update: &'a CheckUpdate,
    /// The lease's `expires_at`.
    pub deadline: Deadline,
}

/// What [`Forge::find_or_create_check`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckFound {
    /// The App's check existed; it was not written.
    Found(CheckRun),
    /// There was none; this one was created.
    Created(CheckRun),
}

/// Why [`Forge::update_check_if`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum CompareAndSetError<E> {
    /// The check's `external_id` is no longer the one read: another writer moved it.
    #[error("the check's external_id changed since it was read")]
    Conflict {
        /// The `external_id` the check carries now.
        found: String,
    },
    /// GitHub could not be read or written.
    #[error(transparent)]
    Forge(E),
}

/// What [`Forge::upsert_comment`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Upserted {
    /// A new comment was posted.
    Created,
    /// The existing comment was edited.
    Updated,
    /// The existing comment already read so; nothing was written.
    Unchanged,
}

/// Who GitHub says made a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitIdentity {
    /// The commit.
    pub commit: CommitId,
    /// Its authors, co-authors included, as GitHub lists them: the git author first, then the
    /// `Co-authored-by:` trailers. The eligibility rule reads the first only (04 §5).
    pub authors: Vec<Actor>,
    /// Its committer.
    pub committer: Actor,
}

/// A git identity as GitHub resolves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Actor {
    /// A GitHub user, by numeric id.
    User(UserId),
    /// No GitHub user is linked to the name and email.
    NotAUser,
}

/// One review on a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// Its id; ids grow with time.
    pub id: NonZeroU64,
    /// Its author; `None` for a deleted account.
    pub reviewer: Option<UserId>,
    /// Its state.
    pub state: ReviewState,
    /// The commit it was submitted on; `None` when GitHub reports none.
    pub commit: Option<CommitId>,
    /// When it was submitted; `None` for a pending review, which GitHub gives no time.
    pub submitted_at: Option<Timestamp>,
}

/// A review's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReviewState {
    /// `APPROVED`.
    Approved,
    /// `CHANGES_REQUESTED`.
    ChangesRequested,
    /// `COMMENTED`.
    Commented,
    /// `DISMISSED`.
    Dismissed,
    /// `PENDING`: started and not submitted.
    Pending,
}

/// What a reaction is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReactionSubject {
    /// An issue comment, such as the summary.
    IssueComment(CommentId),
    /// A review comment on the diff, such as an inline finding.
    ReviewComment(ReviewCommentId),
}

/// A review comment's numeric id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReviewCommentId(pub NonZeroU64);

/// One reaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reaction {
    /// Its id.
    pub id: NonZeroU64,
    /// Who reacted; `None` for a deleted account.
    pub user: Option<UserId>,
    /// The emoji.
    pub content: ReactionContent,
}

/// A reaction's emoji, GitHub's closed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReactionContent {
    /// `+1`.
    ThumbsUp,
    /// `-1`.
    ThumbsDown,
    /// `laugh`.
    Laugh,
    /// `confused`.
    Confused,
    /// `heart`.
    Heart,
    /// `hooray`.
    Hooray,
    /// `rocket`.
    Rocket,
    /// `eyes`.
    Eyes,
}

/// One event of a PR's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEvent {
    /// Its id; `None` for kinds GitHub gives none (`committed`, `cross-referenced`).
    pub id: Option<NonZeroU64>,
    /// Who caused it, when GitHub names an `actor`.
    pub actor: Option<UserId>,
    /// What happened.
    pub kind: TimelineKind,
}

/// The kind of a timeline event. Kinds assay reads carry their data; every other kind is
/// [`TimelineKind::Other`] with its name only, and none of its fields are read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineKind {
    /// `labeled`, with the label's name.
    Labeled(String),
    /// `unlabeled`, with the label's name.
    Unlabeled(String),
    /// `review_dismissed`, with the dismissed review's id.
    ReviewDismissed(NonZeroU64),
    /// `head_ref_force_pushed`.
    HeadRefForcePushed,
    /// `base_ref_changed`.
    BaseRefChanged,
    /// `ready_for_review`.
    ReadyForReview,
    /// `convert_to_draft`.
    ConvertToDraft,
    /// Any other kind, by its `event` name.
    Other(String),
}
