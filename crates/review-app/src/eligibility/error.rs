//! Why the GitHub state snapshot could not be taken, and which port call failed.

use std::fmt;

use thiserror::Error;

use super::JudgeError;
use super::commands::MAX_COMMAND_RECORDS;
use super::identities::RangeError;
use super::pushes::MAX_PUSH_RECORDS;
use crate::publish::CommentId;

/// The port call of the snapshot that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStep {
    /// Listing the ledger's push records.
    PushRecords,
    /// Listing the range with git.
    RangeList,
    /// Checking a merge candidate with git (ancestry, `merge-tree`, the commit's tree).
    MergeCheck,
    /// Reading the range's commit identities from GitHub.
    Identities,
    /// Listing the PR's reviews.
    Reviews,
    /// Listing the ledger's command records.
    CommandRecords,
}

impl fmt::Display for SnapshotStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PushRecords => "list the push records",
            Self::RangeList => "list the range",
            Self::MergeCheck => "check a merge from base",
            Self::Identities => "read the commit identities",
            Self::Reviews => "list the reviews",
            Self::CommandRecords => "list the command records",
        })
    }
}

/// Why no snapshot could be taken. `decide` then fails closed.
#[derive(Debug, Error)]
pub enum StateError<F, G, L, C> {
    /// A GitHub call failed.
    #[error("could not {step} from GitHub")]
    Forge {
        /// The call.
        step: SnapshotStep,
        /// The forge's error.
        #[source]
        source: F,
    },
    /// A git call failed.
    #[error("could not {step} with git")]
    Range {
        /// The call.
        step: SnapshotStep,
        /// git's error.
        #[source]
        source: G,
    },
    /// A ledger call failed.
    #[error("could not {step} from the ledger")]
    Ledger {
        /// The call.
        step: SnapshotStep,
        /// The ledger's error.
        #[source]
        source: L,
    },
    /// A command's comment could not be re-fetched.
    #[error("could not re-fetch a command's comment")]
    Comments(#[source] C),
    /// The ledger holds more than [`MAX_PUSH_RECORDS`] push records for the PR.
    #[error("the PR has more than {MAX_PUSH_RECORDS} push records")]
    TooManyPushRecords,
    /// The ledger holds more than [`MAX_COMMAND_RECORDS`] command records for the PR.
    #[error("the PR has more than {MAX_COMMAND_RECORDS} command records")]
    TooManyCommandRecords,
    /// Two command records name one comment.
    #[error("two command records name comment {}", comment.0)]
    RepeatedCommand {
        /// The comment.
        comment: CommentId,
    },
    /// GitHub's commit identities do not answer the range commit for commit, in order.
    #[error("GitHub's commit identities do not match the PR range")]
    IdentityMismatch,
}

impl<F, G, L, C> StateError<F, G, L, C> {
    /// A closure wrapping a forge error of `step`.
    pub(crate) const fn forge(step: SnapshotStep) -> impl Fn(F) -> Self {
        move |source| Self::Forge { step, source }
    }

    /// A closure wrapping a ledger error of `step`.
    pub(crate) const fn ledger(step: SnapshotStep) -> impl Fn(L) -> Self {
        move |source| Self::Ledger { step, source }
    }

    /// The command judgement's failure as the snapshot's.
    pub(super) fn of_commands(error: JudgeError<L, C>) -> Self {
        match error {
            JudgeError::Ledger(source) => Self::Ledger {
                step: SnapshotStep::CommandRecords,
                source,
            },
            JudgeError::TooMany => Self::TooManyCommandRecords,
            JudgeError::Repeated(comment) => Self::RepeatedCommand { comment },
            JudgeError::Comments(source) => Self::Comments(source),
        }
    }

    /// The range reading's failure as the snapshot's: a failed identity read is
    /// [`SnapshotStep::Identities`]'s.
    pub(crate) fn of_range(error: RangeError<G, F>) -> Self {
        match error {
            RangeError::Range { step, source } => Self::Range { step, source },
            RangeError::Forge(source) => Self::Forge {
                step: SnapshotStep::Identities,
                source,
            },
            RangeError::IdentityMismatch => Self::IdentityMismatch,
        }
    }
}
