//! Why `run_decide` wrote no outputs (exit 2): nothing is published, and the check `open` left
//! `in_progress` is closed as `failure publish-error` by the reconcile (06 §4).

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use assay_app::{
    BaseIdentitiesError, BundleSource, CommandComments, DecideError, Forge, InputsError, Ledger,
    LedgerReadError, LedgerSnapshotError, PrepareError, PriorCarryError, PullRequestState,
    RunArtifacts, StateError, TargetError,
};

use crate::admit::{FetchCommits, FetchedSide};
use crate::prior::PriorError;
use crate::pull_request::ForgeObjectsError;

/// Why `decide` wrote no outputs. `H`, `S`, `A`, `F`, `L`, `B` and `C` are the fetch's, the
/// live state's, the artifacts', GitHub's, the ledger's, the bundle store's and the comments'
/// errors.
#[derive(Debug)]
pub enum DecideRunError<H, S, A, F, L, B, C> {
    /// plan's outputs name no plan artifact.
    NoPlanArtifact,
    /// A pinned commit could not be fetched.
    Fetch {
        /// Which one.
        side: FetchedSide,
        /// Why.
        source: H,
    },
    /// The run's inputs could not be read to authenticate them; a refusal is not this.
    Authenticate(DecideError<ForgeObjectsError, S, A>),
    /// The base P10 identity lists were refused or could not be read.
    Lists(BaseIdentitiesError<ForgeObjectsError>),
    /// The PR's ledger state could not be read.
    State(LedgerReadError<L>),
    /// The previous completed run's head could not be read from its bundle.
    PriorHead(PriorCarryError<B>),
    /// How the previous head stands to head could not be read.
    Target(TargetError<ForgeObjectsError>),
    /// The range-diff's git process group may still be running.
    RangeDiffKilled,
    /// The previous completed run's carried findings or waivers could not be read.
    Prior(PriorCarryError<B>),
    /// The GitHub state snapshot could not be taken.
    Snapshot(Box<StateError<F, ForgeObjectsError, LedgerSnapshotError<L>, C>>),
    /// The decision inputs could not be composed.
    Inputs(InputsError),
    /// The bundle could not be built.
    Prepare(PrepareError),
    /// `--out` or a file in it could not be written.
    Write {
        /// The directory or file.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
}

/// The [`DecideRunError`] of `run_decide` over the ports' error types.
pub type DecideRunFailure<H, S, A, F, L, B, C> = DecideRunError<
    <H as FetchCommits>::Error,
    <S as PullRequestState>::Error,
    <A as RunArtifacts>::Error,
    <F as Forge>::Error,
    <L as Ledger>::Error,
    <B as BundleSource>::Error,
    <C as CommandComments>::Error,
>;

impl<H, S, A, F, L, B, C> From<PriorError<L, B>> for DecideRunError<H, S, A, F, L, B, C> {
    fn from(error: PriorError<L, B>) -> Self {
        match error {
            PriorError::State(source) => Self::State(source),
            PriorError::PriorHead(source) => Self::PriorHead(source),
            PriorError::Target(source) => Self::Target(source),
            PriorError::RangeDiffKilled => Self::RangeDiffKilled,
        }
    }
}

impl<H, S, A, F, L, B, C> fmt::Display for DecideRunError<H, S, A, F, L, B, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlanArtifact => f.write_str("plan's outputs name no plan artifact"),
            Self::Fetch { side, .. } => match side {
                FetchedSide::Base => f.write_str("could not fetch the base commit"),
                FetchedSide::Head => f.write_str("could not fetch the head commit"),
            },
            Self::Authenticate(_) => f.write_str("could not authenticate the run's inputs"),
            Self::Lists(_) => f.write_str("could not read the base identity lists"),
            Self::State(_) => f.write_str("could not read the PR's ledger state"),
            Self::PriorHead(_) => f.write_str("could not read the previous run's head"),
            Self::Target(_) => f.write_str("could not read how the previous head stands to head"),
            Self::RangeDiffKilled => {
                f.write_str("the range-diff's git process group may still be running")
            }
            Self::Prior(_) => f.write_str("could not read the previous run's carry"),
            Self::Snapshot(_) => f.write_str("could not take the GitHub state snapshot"),
            Self::Inputs(_) => f.write_str("could not compose the decision inputs"),
            Self::Prepare(_) => f.write_str("could not build the bundle"),
            Self::Write { path, .. } => write!(f, "cannot write {}", path.display()),
        }
    }
}

impl<H, S, A, F, L, B, C> Error for DecideRunError<H, S, A, F, L, B, C>
where
    H: Error + 'static,
    S: Error + 'static,
    A: Error + 'static,
    F: Error + 'static,
    L: Error + 'static,
    B: Error + 'static,
    C: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Fetch { source, .. } => Some(source),
            Self::Authenticate(source) => Some(source),
            Self::Lists(source) => Some(source),
            Self::State(source) => Some(source),
            Self::PriorHead(source) | Self::Prior(source) => Some(source),
            Self::Target(source) => Some(source),
            Self::NoPlanArtifact | Self::RangeDiffKilled => None,
            Self::Snapshot(source) => Some(source.as_ref()),
            Self::Inputs(source) => Some(source),
            Self::Prepare(source) => Some(source),
            Self::Write { source, .. } => Some(source),
        }
    }
}
