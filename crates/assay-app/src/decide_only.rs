//! The GitHub ports decide-only needs beyond the pipeline's (#61 DO-9): replies to command
//! comments (D10), the base tip's eligibility lists (D4) and the hourly poll's reads (D8).
//! Implemented by `assay-forge`'s `GitHub`.

use std::borrow::Cow;
use std::num::NonZeroU64;

use crate::{
    CheckName, CommandRejected, CommentId, CommitId, ObservedConclusion, Review, Timestamp,
};

/// Replies to command comments, written with the `assay-verdict` App's token.
pub trait CommandReplies {
    /// Why the reply could not be posted.
    type Error: std::error::Error;

    /// Posts `reply` on PR `pr` as the answer to comment `to`: the body is
    /// [`Reply::body`], so it carries the comment's marker and assay's sentence, never the
    /// comment's text (J8). A post retried after a retryable failure is not posted twice.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the reply cannot be posted.
    fn reply(&self, pr: NonZeroU64, to: CommentId, reply: Reply) -> Result<(), Self::Error>;
}

/// What assay answers to a command it did not act on (D10): a closed set, each one fixed
/// sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reply {
    /// The command was not recorded, for this closed reason.
    ///
    /// Only for an `/assay` command line a person posted, so a caller passes one of
    /// [`CommandRejected::Unknown`], [`CommandRejected::Malformed`],
    /// [`CommandRejected::Ineligible`], [`CommandRejected::NoPushClock`],
    /// [`CommandRejected::BeforePushClock`] and [`CommandRejected::ViaApp`]. The other three
    /// never get a reply (D10): [`CommandRejected::NotACommand`] is not a command,
    /// [`CommandRejected::NotCreated`] is an edit or deletion, which is never read as one, and
    /// [`CommandRejected::NoAuthor`] has no author to answer. Their sentences still render, so
    /// a caller that passes one posts closed text, never the comment's.
    Rejected(CommandRejected),
    /// The author has used up their commands on this PR (D6).
    RateLimited,
    /// `/assay retry` is not available yet (D7).
    RetryUnavailable,
    /// The PR's head has no evidence pointer or publication register to decide from.
    NoEvidence,
    /// The stored evidence was decided under another rubric version (D11).
    RerunNeeded,
    /// The PR moved since its evidence was decided; its next run decides.
    HeadMoved,
    /// The command could not be recorded in the ledger, so it has no effect (06 §1).
    NotRecorded,
}

impl Reply {
    /// The hidden marker of the reply to comment `to`: `<!-- assay-reply <id> -->`.
    #[must_use]
    pub fn marker(to: CommentId) -> String {
        format!("<!-- assay-reply {} -->", to.0)
    }

    /// The reply's sentence: fixed text, or for [`Reply::Rejected`] the closed reason's.
    #[must_use]
    pub fn sentence(self) -> Cow<'static, str> {
        match self {
            Self::Rejected(reason) => {
                Cow::Owned(format!("assay did not record this command: {reason}."))
            }
            Self::RateLimited => Cow::Borrowed(
                "assay did not act on this command: its author has used every command allowed \
                 on this PR.",
            ),
            Self::RetryUnavailable => Cow::Borrowed(
                "assay did not act on this command: retrying a role is not available yet.",
            ),
            Self::NoEvidence => Cow::Borrowed(
                "assay did not act on this command: this PR has no completed assay run on its \
                 head to decide again.",
            ),
            Self::RerunNeeded => Cow::Borrowed(
                "assay did not act on this command: the last run was decided under another \
                 rubric version, so a new run is needed.",
            ),
            Self::HeadMoved => Cow::Borrowed(
                "assay did not act on this command: the PR changed since its last run, and its \
                 next run decides.",
            ),
            Self::NotRecorded => Cow::Borrowed(
                "assay could not record this command, so it has no effect; post it again.",
            ),
        }
    }

    /// The reply's comment body: [`Reply::marker`] on the first line, which is never a
    /// command line, then [`Reply::sentence`].
    #[must_use]
    pub fn body(self, to: CommentId) -> String {
        format!("{}\n{}", Self::marker(to), self.sentence())
    }
}

/// The largest `.assay/owners` or `.assay/agents` read from a base tip, in bytes.
pub const MAX_TIP_LIST_BYTES: usize = 64 * 1024;

/// The base tip's eligibility list files as stored, each `None` when absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TipListFiles {
    /// `.assay/owners` ([`OWNERS_PATH`](crate::OWNERS_PATH)).
    pub owners: Option<Vec<u8>>,
    /// `.assay/agents` ([`AGENTS_PATH`](crate::AGENTS_PATH)).
    pub agents: Option<Vec<u8>>,
}

/// The base branch's current eligibility lists, read to narrow the recorded ones (R3-26, D4).
pub trait TipLists {
    /// Why the lists could not be read, or were refused.
    type Error: std::error::Error;

    /// [`OWNERS_PATH`](crate::OWNERS_PATH) and [`AGENTS_PATH`](crate::AGENTS_PATH) at
    /// `base_ref`'s tip, each at most [`MAX_TIP_LIST_BYTES`]. A file that is absent is `None`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when either cannot be read, is over the cap, or is not a regular file (a
    /// symlink or a directory).
    fn lists_at(&self, base_ref: &str) -> Result<TipListFiles, Self::Error>;
}

/// The most open PRs the hourly poll reads (D8).
pub const MAX_POLL_PULLS: usize = 50;

/// An open PR as the poll reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolledPull {
    /// Its number.
    pub number: NonZeroU64,
    /// Its head commit.
    pub head_sha: CommitId,
    /// Its base branch, which names its verdict check ([`CheckName::for_base`]).
    pub base_ref: String,
}

/// The App's verdict check on a head, as the poll reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdictView {
    /// Queued or in progress.
    Pending,
    /// Completed.
    Completed(CompletedVerdict),
}

/// A completed verdict check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedVerdict {
    /// Its conclusion.
    pub conclusion: ObservedConclusion,
    /// Its `output.title`, if set: the closed reason code of a failure (#218).
    pub title: Option<String>,
    /// When it completed, comparable with a review's `submitted_at`.
    pub completed_at: Timestamp,
}

/// The hourly poll's reads, with a read-only token (D8).
pub trait PollForge {
    /// Why GitHub could not be read.
    type Error: std::error::Error;

    /// The repository's open PRs, at most `limit` of them, in GitHub's order.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the PRs cannot be listed.
    fn open_pulls(&self, limit: usize) -> Result<Vec<PolledPull>, Self::Error>;

    /// The check named `name` on `head` that the `assay-verdict` App authored, by its app id;
    /// a same-named check of any other app is never returned.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the checks cannot be listed, or the App has two of that name.
    fn verdict_check(
        &self,
        head: &CommitId,
        name: &CheckName,
    ) -> Result<Option<VerdictView>, Self::Error>;

    /// Every review on PR `number`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the reviews cannot be listed.
    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, Self::Error>;
}
