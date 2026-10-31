//! `assay guard`'s rules (#60 ST-2, 0006): whether the pipeline runs for an event, and the run
//! identity every later job reads from its outputs (I2).
//!
//! A PR whose head is not in this repository, a deleted one included, is refused before any
//! job holding a secret runs (I3); so is a dispatch for a closed PR. An `edited` event that did
//! not change the base does not run. Everything else runs with the identity read from the
//! payload, or for a `workflow_dispatch` (which names only a PR) from the PR itself.

use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;

use assay_forge::github::{
    DispatchPayload, EventPayload, HeadRepository, PullIdentity, PullRequestAction,
    PullRequestPayload, PullState,
};

use crate::ci::OutputName;
use crate::outputs::{Identity, Trigger};

/// What `guard` concluded for an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guarded {
    /// The pipeline runs, with this identity: exit 0 once its outputs are written.
    Run(Identity),
    /// The event is not a reason to run (`edited` without a base change): `run=false` is
    /// written, and nothing else.
    Skip,
    /// The event is refused: nothing is written, and the job fails (exit 1, 0006).
    Refused(Refusal),
}

impl Guarded {
    /// The outputs to write: the identity's for a run, `run=false` alone for a skip, and none
    /// for a refusal.
    #[must_use]
    pub fn outputs(&self) -> Vec<(OutputName, String)> {
        match self {
            Self::Run(identity) => identity.outputs(),
            Self::Skip => vec![(OutputName::Run, "false".to_owned())],
            Self::Refused(_) => Vec::new(),
        }
    }
}

/// Why `guard` refused to run for an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The PR's head is not in this repository (0006).
    UntrustedHead {
        /// The PR.
        pr: NonZeroU64,
        /// Where its head is.
        head: UntrustedHead,
    },
    /// A dispatch names a closed PR.
    Closed {
        /// The PR.
        pr: NonZeroU64,
    },
}

/// Where an untrusted head is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UntrustedHead {
    /// Another repository, by id.
    Other(NonZeroU64),
    /// GitHub names no head repository: it was deleted, so nothing says it is this one.
    Deleted,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UntrustedHead {
                pr,
                head: UntrustedHead::Other(repository),
            } => write!(
                f,
                "untrusted head repository: PR #{pr}'s head is in repository {repository}, not \
                 this one; assay does not run for it (0006)"
            ),
            Self::UntrustedHead {
                pr,
                head: UntrustedHead::Deleted,
            } => write!(
                f,
                "untrusted head repository: PR #{pr}'s head repository was deleted; assay does \
                 not run for it (0006)"
            ),
            Self::Closed { pr } => write!(f, "PR #{pr} is closed; assay runs only for open PRs"),
        }
    }
}

/// Why `guard` could not decide.
#[derive(Debug, PartialEq, Eq)]
pub enum GuardError<E> {
    /// The payload is for another repository than `GITHUB_REPOSITORY_ID`'s.
    OtherRepository {
        /// The payload's `repository.id`.
        payload: NonZeroU64,
    },
    /// The PR a dispatch names could not be read.
    Pull {
        /// The PR.
        pr: NonZeroU64,
        /// The read's failure.
        source: E,
    },
    /// The base branch of the PR a dispatch names is empty or holds a control character, so
    /// it is not written as an output.
    BaseRef {
        /// The PR.
        pr: NonZeroU64,
    },
}

impl<E> fmt::Display for GuardError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OtherRepository { payload } => write!(
                f,
                "the event payload is for repository {payload}, not GITHUB_REPOSITORY_ID's"
            ),
            Self::Pull { pr, .. } => write!(f, "reading PR #{pr} failed"),
            Self::BaseRef { pr } => write!(
                f,
                "PR #{pr}'s base branch is empty or holds a control character"
            ),
        }
    }
}

impl<E: Error + 'static> Error for GuardError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Pull { source, .. } => Some(source),
            Self::OtherRepository { .. } | Self::BaseRef { .. } => None,
        }
    }
}

/// Decides whether the pipeline runs for `event` in the repository `repository_id`.
///
/// `repository_id` is `GITHUB_REPOSITORY_ID`. A dispatch's PR is read with `read_pull`, which
/// is not called for a `pull_request_target` event: its payload carries the PR.
///
/// The head is checked before the `edited` filter, so every event of a PR whose head is not
/// this repository's is refused (fail closed, 0006).
///
/// # Errors
///
/// [`GuardError::OtherRepository`] when the payload is for another repository (nothing is
/// read); [`GuardError::Pull`] when the dispatch's PR cannot be read; and
/// [`GuardError::BaseRef`] when its base branch is not a valid output value.
pub fn guard<E>(
    repository_id: NonZeroU64,
    event: EventPayload,
    read_pull: impl FnOnce(NonZeroU64) -> Result<PullIdentity, E>,
) -> Result<Guarded, GuardError<E>> {
    match event {
        EventPayload::PullRequest(payload) => pull_request(repository_id, payload),
        EventPayload::Dispatch(payload) => dispatch(repository_id, payload, read_pull),
    }
}

/// A `pull_request_target` event, whose payload carries the PR.
fn pull_request<E>(
    repository_id: NonZeroU64,
    payload: PullRequestPayload,
) -> Result<Guarded, GuardError<E>> {
    same_repository(repository_id, payload.repository_id)?;
    let pr = payload.number;
    // `HeadRepository::Base` is the payload's repository, shown above to be this one.
    let head = match payload.head_repository {
        HeadRepository::Base => None,
        HeadRepository::Other(repository) => Some(UntrustedHead::Other(repository)),
        HeadRepository::Deleted => Some(UntrustedHead::Deleted),
    };
    if let Some(head) = head {
        return Ok(Guarded::Refused(Refusal::UntrustedHead { pr, head }));
    }
    let trigger = match payload.action {
        PullRequestAction::Opened => Trigger::Opened,
        PullRequestAction::Synchronize { before } => Trigger::Synchronize { before },
        PullRequestAction::Reopened => Trigger::Reopened,
        PullRequestAction::ReadyForReview => Trigger::ReadyForReview,
        PullRequestAction::Edited { base_changed: true } => Trigger::Edited,
        PullRequestAction::Edited {
            base_changed: false,
        } => return Ok(Guarded::Skip),
    };
    Ok(Guarded::Run(Identity {
        pr,
        head_sha: payload.head_sha,
        base_sha: payload.base_sha,
        base_ref: payload.base_ref,
        draft: payload.draft,
        author: payload.author,
        sender: payload.sender,
        trigger,
        default_branch: payload.default_branch,
    }))
}

/// A `workflow_dispatch` event, which names only the PR: it is read with `read_pull`.
fn dispatch<E>(
    repository_id: NonZeroU64,
    payload: DispatchPayload,
    read_pull: impl FnOnce(NonZeroU64) -> Result<PullIdentity, E>,
) -> Result<Guarded, GuardError<E>> {
    same_repository(repository_id, payload.repository_id)?;
    let pr = payload.pr;
    let pull = read_pull(pr).map_err(|source| GuardError::Pull { pr, source })?;
    let head = match pull.head_repo {
        Some(repository) if repository == repository_id => None,
        Some(repository) => Some(UntrustedHead::Other(repository)),
        None => Some(UntrustedHead::Deleted),
    };
    if let Some(head) = head {
        return Ok(Guarded::Refused(Refusal::UntrustedHead { pr, head }));
    }
    if pull.state == PullState::Closed {
        return Ok(Guarded::Refused(Refusal::Closed { pr }));
    }
    if pull.base_ref.is_empty() || pull.base_ref.chars().any(char::is_control) {
        return Err(GuardError::BaseRef { pr });
    }
    Ok(Guarded::Run(Identity {
        pr,
        head_sha: pull.head_sha,
        base_sha: pull.base_sha,
        base_ref: pull.base_ref,
        draft: pull.draft,
        author: pull.author,
        sender: payload.sender,
        trigger: Trigger::Dispatch,
        default_branch: payload.default_branch,
    }))
}

/// Refuses a payload whose repository (`payload`) is not `repository_id`.
const fn same_repository<E>(
    repository_id: NonZeroU64,
    payload: NonZeroU64,
) -> Result<(), GuardError<E>> {
    if payload.get() == repository_id.get() {
        Ok(())
    } else {
        Err(GuardError::OtherRepository { payload })
    }
}
