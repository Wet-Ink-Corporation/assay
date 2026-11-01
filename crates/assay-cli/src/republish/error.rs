//! Why `run_republish` published nothing (exit 2): the check stays as it was, and the
//! reconcile closes one left `in_progress` (06 §4).

use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_app::{
    Ledger, LedgerReadError, PublishError, PublishLedger, RedecidedError, VerdictForge,
};

use crate::artifacts::DirArtifactsError;
use crate::redecide::RedecideStateFileError;

/// Why `decide-only publish` published nothing.
///
/// `G` is GitHub's error (the live PR, the default branch, the check and the summary), `L` the
/// ledger files' and `P` the publish ledger's.
#[derive(Debug)]
pub enum RepublishRunError<G, L, P> {
    /// `ASSAY_PR` is not one of the PRs guard wrote in `prs`.
    NotScheduled {
        /// `ASSAY_PR`.
        pr: NonZeroU64,
    },
    /// decide's state or bundle is not there.
    Missing {
        /// The file.
        path: PathBuf,
    },
    /// decide's state or bundle is over its cap; it was not read whole.
    TooLarge {
        /// The file.
        path: PathBuf,
    },
    /// decide's `--out` or a file in it could not be read, or is not a directory or regular
    /// file.
    Read(DirArtifactsError),
    /// decide's state was refused.
    State(RedecideStateFileError),
    /// The bundle does not hash to the digest decide's state names.
    Digest,
    /// The PR's live head and base could not be read.
    Live(G),
    /// The repository's default branch could not be read.
    DefaultBranch(G),
    /// The PR's ledger state could not be read.
    Ledger(LedgerReadError<L>),
    /// The refusal could not be published.
    Refusal(PublishError<G, P>),
    /// The re-decision could not be published, or its bundle is not this run's re-decision.
    Decided(RedecidedError<G, P>),
}

/// The [`RepublishRunError`] of `run_republish` over its ports.
pub type RepublishRunFailure<A, L, P> = RepublishRunError<
    <A as VerdictForge>::Error,
    <L as Ledger>::Error,
    <P as PublishLedger>::Error,
>;

impl<G, L, P> fmt::Display for RepublishRunError<G, L, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotScheduled { pr } => write!(f, "ASSAY_PR {pr} is not one of guard's prs"),
            Self::Missing { path } => write!(f, "{} is not there", path.display()),
            Self::TooLarge { path } => write!(f, "{} is over its size cap", path.display()),
            Self::Read(_) => f.write_str("could not read decide's output"),
            Self::State(_) => f.write_str("decide's state was refused"),
            Self::Digest => {
                f.write_str("the decision bundle does not hash to the digest decide's state names")
            }
            Self::Live(_) => f.write_str("could not read the PR's live head and base"),
            Self::DefaultBranch(_) => f.write_str("could not read the default branch"),
            Self::Ledger(_) => f.write_str("could not read the PR's ledger state"),
            Self::Refusal(_) => f.write_str("could not publish the refusal"),
            Self::Decided(_) => f.write_str("could not publish the re-decision"),
        }
    }
}

impl<G, L, P> Error for RepublishRunError<G, L, P>
where
    G: Error + 'static,
    L: Error + 'static,
    P: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotScheduled { .. }
            | Self::Missing { .. }
            | Self::TooLarge { .. }
            | Self::Digest => None,
            Self::Read(source) => Some(source),
            Self::State(source) => Some(source),
            Self::Live(source) | Self::DefaultBranch(source) => Some(source),
            Self::Ledger(source) => Some(source),
            Self::Refusal(source) => Some(source),
            Self::Decided(source) => Some(source),
        }
    }
}
