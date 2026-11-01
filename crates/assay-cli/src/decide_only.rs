//! `assay decide-only guard`'s rules (#61 DO-10, J3, 0006, D8 A): which PRs a decide-only run
//! decides again for an event, read with no secret and no write token.
//!
//! A command comment names its PR, read through [`DecideOnlyGuardReads::pull_identity`] since
//! an `issue_comment` payload has no head repository; a label event carries its PR; the hourly
//! poll selects with [`assay_app::poll_pending`]. A PR whose head is not in this repository, a
//! deleted one included, is refused for a comment or a label (exit 1), and left out of the
//! poll, so no PR with an untrusted head reaches the job that holds a secret (0006). Everything
//! that is no reason to decide is a no-op that reads nothing: `prs=[]`.

use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;

use assay_app::{
    CommentAction, Parsed, PollError, PollForge, PrerequisiteForge, UserId, parse_command,
    poll_pending,
};
use assay_forge::github::{
    CommentPayload, DecideOnlyEvent, ForgeError, GitHub, HeadRepository, Label, LabelPayload,
    PullIdentity, PullState, Sender, SenderKind, Sleep, Transport,
};

use crate::ci::OutputName;
use crate::guard::{Refusal, UntrustedHead};
use crate::outputs::{
    DECIDE_ONLY_SKIP, DecideOnlyGuardOutputs, DecideOnlyPrs, DecideTrigger, PrsError,
};

/// The prefix of every label of assay's.
const ASSAY_LABEL: &str = "assay:";

/// The GitHub reads `decide-only guard` makes, besides the poll's: all with the job's
/// read-only `github.token`.
pub trait DecideOnlyGuardReads: PollForge<Error: Error + 'static> {
    /// PR `pr`: its head repository and whether it is open.
    ///
    /// # Errors
    ///
    /// [`PollForge::Error`] when it cannot be read exactly.
    fn pull_identity(&self, pr: NonZeroU64) -> Result<PullIdentity, Self::Error>;

    /// The repository's default branch, which names its verdict check.
    ///
    /// # Errors
    ///
    /// [`PollForge::Error`] when it cannot be read exactly.
    fn default_branch(&self) -> Result<String, Self::Error>;
}

impl<T: Transport<Error: 'static>, S: Sleep> DecideOnlyGuardReads for GitHub<T, S> {
    fn pull_identity(&self, pr: NonZeroU64) -> Result<PullIdentity, ForgeError<T::Error>> {
        Self::pull_identity(self, pr)
    }

    fn default_branch(&self) -> Result<String, ForgeError<T::Error>> {
        PrerequisiteForge::default_branch(self)
    }
}

/// What a decide-only guard runs for: this repository and the `assay-verdict` App's bot user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecideOnlyGuardJob {
    /// `GITHUB_REPOSITORY_ID`.
    pub repository_id: NonZeroU64,
    /// The App's `<slug>[bot]` user (`ASSAY_VERDICT_BOT_ID`): nothing it does is a reason to
    /// decide again.
    pub app_bot: UserId,
}

/// What the decide-only guard concluded for an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedDecideOnly {
    /// Decide these PRs again: exit 0 once `prs` and `trigger` are written.
    Run(DecideOnlyGuardOutputs),
    /// Nothing to decide: `prs=[]` alone is written, exit 0.
    Skip(Skipped),
    /// The event is refused: nothing is written, and the job fails (exit 1, 0006).
    Refused(Refusal),
}

impl GuardedDecideOnly {
    /// The outputs to write: `prs` and `trigger` for a run, `prs=[]` alone for a skip, and none
    /// for a refusal.
    #[must_use]
    pub fn outputs(&self) -> Vec<(OutputName, String)> {
        match self {
            Self::Run(outputs) => outputs.outputs(),
            Self::Skip(_) => DECIDE_ONLY_SKIP
                .iter()
                .map(|&(name, value)| (name, value.to_owned()))
                .collect(),
            Self::Refused(_) => Vec::new(),
        }
    }
}

/// Why an event is no reason to decide again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Skipped {
    /// The comment is on an issue that is not a PR.
    NotAPullRequest,
    /// The comment was edited or deleted: only a new comment is read as a command.
    NotCreated,
    /// A Bot sent the event.
    Bot,
    /// The `assay-verdict` App's bot user sent the event.
    AppSender,
    /// The comment's first line is not an `/assay` command line.
    NotACommand,
    /// The PR commented on is closed.
    Closed,
    /// The label is not one of assay's (`assay:*`).
    NotAssayLabel,
    /// The poll found no PR waiting on an approval that has arrived.
    NothingPending,
}

/// Why the decide-only guard could not decide.
#[derive(Debug, PartialEq, Eq)]
pub enum GuardDecideOnlyError<E> {
    /// The payload is for another repository than `GITHUB_REPOSITORY_ID`'s.
    OtherRepository {
        /// The payload's `repository.id`.
        payload: NonZeroU64,
    },
    /// The PR a command comment is on could not be read.
    Pull {
        /// The PR.
        pr: NonZeroU64,
        /// The read's failure.
        source: E,
    },
    /// The repository's default branch could not be read for the poll.
    DefaultBranch(E),
    /// The poll could not read GitHub.
    Poll(PollError<E>),
    /// The poll's selection is not a set of PRs one run may decide (a PR selected twice, or
    /// more than the cap): nothing is written rather than a set GitHub's answers made up.
    Selection(PrsError),
}

impl<E> fmt::Display for GuardDecideOnlyError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OtherRepository { payload } => write!(
                f,
                "the event payload is for repository {payload}, not GITHUB_REPOSITORY_ID's"
            ),
            Self::Pull { pr, .. } => write!(f, "reading PR #{pr} failed"),
            Self::DefaultBranch(_) => {
                f.write_str("reading the repository's default branch for the hourly poll failed")
            }
            Self::Poll(_) => f.write_str("the hourly poll could not read GitHub"),
            Self::Selection(_) => f.write_str("the hourly poll's selection was refused"),
        }
    }
}

impl<E: Error + 'static> Error for GuardDecideOnlyError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OtherRepository { .. } => None,
            Self::Pull { source, .. } | Self::DefaultBranch(source) => Some(source),
            Self::Poll(source) => Some(source),
            Self::Selection(source) => Some(source),
        }
    }
}

/// Decides which PRs a decide-only run decides again for `event`.
///
/// Nothing is read for an event that is no reason to decide, or for another repository's
/// payload; a command comment reads its PR, and the poll reads the default branch, the open
/// PRs and then each selected PR, which is left out unless open with its head here.
///
/// # Errors
///
/// [`GuardDecideOnlyError::OtherRepository`] when the payload is for another repository;
/// [`GuardDecideOnlyError::Pull`], [`GuardDecideOnlyError::DefaultBranch`] and
/// [`GuardDecideOnlyError::Poll`] when a read fails; [`GuardDecideOnlyError::Selection`] when
/// the poll selects a PR twice or more than
/// [`MAX_DECIDE_ONLY_PRS`](crate::outputs::MAX_DECIDE_ONLY_PRS).
pub fn guard_decide_only<G: DecideOnlyGuardReads>(
    job: DecideOnlyGuardJob,
    event: &DecideOnlyEvent,
    reads: &G,
) -> Result<GuardedDecideOnly, GuardDecideOnlyError<G::Error>> {
    match event {
        DecideOnlyEvent::Comment(payload) => comment(job, payload, reads),
        DecideOnlyEvent::Label(payload) => label(job, payload),
        DecideOnlyEvent::Schedule { repository_id } => {
            same_repository(job, *repository_id)?;
            poll(job, reads)
        }
    }
}

/// An `issue_comment`: an `/assay` line a person posted on an open PR whose head is here.
fn comment<G: DecideOnlyGuardReads>(
    job: DecideOnlyGuardJob,
    payload: &CommentPayload,
    reads: &G,
) -> Result<GuardedDecideOnly, GuardDecideOnlyError<G::Error>> {
    same_repository(job, payload.repository_id)?;
    let Some(pr) = payload.pr else {
        return Ok(GuardedDecideOnly::Skip(Skipped::NotAPullRequest));
    };
    if payload.event.action != CommentAction::Created {
        return Ok(GuardedDecideOnly::Skip(Skipped::NotCreated));
    }
    if let Some(skipped) = sent_by_a_bot(job, payload.sender) {
        return Ok(GuardedDecideOnly::Skip(skipped));
    }
    if parse_command(&payload.event.comment.body) == Parsed::NotACommand {
        return Ok(GuardedDecideOnly::Skip(Skipped::NotACommand));
    }
    let pull = reads
        .pull_identity(pr)
        .map_err(|source| GuardDecideOnlyError::Pull { pr, source })?;
    if let Some(head) = untrusted(job, &pull) {
        return Ok(GuardedDecideOnly::Refused(Refusal::UntrustedHead {
            pr,
            head,
        }));
    }
    if pull.state == PullState::Closed {
        return Ok(GuardedDecideOnly::Skip(Skipped::Closed));
    }
    Ok(run_one(pr, DecideTrigger::Comment))
}

/// A `pull_request_target` `labeled` or `unlabeled`: the head is checked first, from the
/// payload, so every label event on an untrusted head is refused (fail closed, as `guard`).
fn label<E>(
    job: DecideOnlyGuardJob,
    payload: &LabelPayload,
) -> Result<GuardedDecideOnly, GuardDecideOnlyError<E>> {
    same_repository(job, payload.repository_id)?;
    let pr = payload.number;
    // `HeadRepository::Base` is the payload's repository, shown above to be this one.
    let head = match payload.head_repository {
        HeadRepository::Base => None,
        HeadRepository::Other(repository) => Some(UntrustedHead::Other(repository)),
        HeadRepository::Deleted => Some(UntrustedHead::Deleted),
    };
    if let Some(head) = head {
        return Ok(GuardedDecideOnly::Refused(Refusal::UntrustedHead {
            pr,
            head,
        }));
    }
    let assays = match &payload.label {
        Label::Name(name) => name.as_str().starts_with(ASSAY_LABEL),
        Label::OverCap { .. } => false,
    };
    if !assays {
        return Ok(GuardedDecideOnly::Skip(Skipped::NotAssayLabel));
    }
    if let Some(skipped) = sent_by_a_bot(job, payload.sender) {
        return Ok(GuardedDecideOnly::Skip(skipped));
    }
    Ok(run_one(pr, DecideTrigger::Label))
}

/// The hourly poll: the selected PRs that are open with their head here, in the poll's order.
fn poll<G: DecideOnlyGuardReads>(
    job: DecideOnlyGuardJob,
    reads: &G,
) -> Result<GuardedDecideOnly, GuardDecideOnlyError<G::Error>> {
    let default_branch = reads
        .default_branch()
        .map_err(GuardDecideOnlyError::DefaultBranch)?;
    let mut prs = Vec::new();
    for pr in poll_pending(reads, &default_branch).map_err(GuardDecideOnlyError::Poll)? {
        let pull = reads
            .pull_identity(pr)
            .map_err(|source| GuardDecideOnlyError::Pull { pr, source })?;
        if untrusted(job, &pull).is_none() && pull.state == PullState::Open {
            prs.push(pr);
        }
    }
    match DecideOnlyPrs::try_from(prs) {
        Ok(prs) => Ok(GuardedDecideOnly::Run(DecideOnlyGuardOutputs::new(
            prs,
            DecideTrigger::Schedule,
        ))),
        Err(PrsError::Empty) => Ok(GuardedDecideOnly::Skip(Skipped::NothingPending)),
        Err(refused @ (PrsError::Repeated { .. } | PrsError::OverCap { .. })) => {
            Err(GuardDecideOnlyError::Selection(refused))
        }
    }
}

/// A run for the one PR `pr`.
fn run_one(pr: NonZeroU64, trigger: DecideTrigger) -> GuardedDecideOnly {
    GuardedDecideOnly::Run(DecideOnlyGuardOutputs::new(DecideOnlyPrs::one(pr), trigger))
}

/// Why `sender` is no one to act for: the App's bot user, or any Bot.
fn sent_by_a_bot(job: DecideOnlyGuardJob, sender: Sender) -> Option<Skipped> {
    if sender.id == job.app_bot {
        Some(Skipped::AppSender)
    } else if sender.kind == SenderKind::Bot {
        Some(Skipped::Bot)
    } else {
        None
    }
}

/// Where `pull`'s head is when it is not in this repository.
fn untrusted(job: DecideOnlyGuardJob, pull: &PullIdentity) -> Option<UntrustedHead> {
    match pull.head_repo {
        Some(repository) if repository == job.repository_id => None,
        Some(repository) => Some(UntrustedHead::Other(repository)),
        None => Some(UntrustedHead::Deleted),
    }
}

/// Refuses a payload whose repository (`payload`) is not this one.
fn same_repository<E>(
    job: DecideOnlyGuardJob,
    payload: NonZeroU64,
) -> Result<(), GuardDecideOnlyError<E>> {
    if payload == job.repository_id {
        Ok(())
    } else {
        Err(GuardDecideOnlyError::OtherRepository { payload })
    }
}
