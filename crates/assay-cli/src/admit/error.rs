//! Why `run_admit` wrote no outputs, or only the slot it holds.

use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use assay_app::{
    AdmitError, BaseIdentitiesError, BundleSource, CommandComments, Forge, LedgerReadError,
    LedgerRefs, PlanDocument, PriorCarryError, RangeError, TargetError,
};

use super::FetchCommits;
use crate::outputs::HeldSlot;
use crate::prior::PriorError;
use crate::pull_request::ForgeObjectsError;

/// Which pinned commit a fetch was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchedSide {
    /// `base_sha`.
    Base,
    /// `head_sha`.
    Head,
}

/// Why base `.assay/suppress.toml` could not be read. It comes from the protected base, so a
/// message may name it.
#[derive(Debug)]
pub enum SuppressError {
    /// The base could not be listed or the file read.
    Objects(ForgeObjectsError),
    /// It is a symlink, submodule or directory, which is never followed.
    NotARegularFile,
    /// It is larger than the pack carries; it was not read.
    TooLarge {
        /// Its listed size.
        size: u64,
    },
    /// The object port answered the read with other than one blob.
    Unread,
    /// It is not UTF-8.
    NotUtf8,
}

impl fmt::Display for SuppressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = super::SUPPRESS;
        match self {
            Self::Objects(_) => write!(f, "could not read {path} in the base"),
            Self::NotARegularFile => write!(f, "{path} in the base is not a regular file"),
            Self::TooLarge { size } => write!(
                f,
                "{path} in the base is {size} bytes, over {}",
                assay_app::MAX_SUPPRESS_BYTES
            ),
            Self::Unread => write!(f, "{path} in the base was not read back"),
            Self::NotUtf8 => write!(f, "{path} in the base is not UTF-8"),
        }
    }
}

impl Error for SuppressError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Objects(source) => Some(source),
            Self::NotARegularFile | Self::TooLarge { .. } | Self::Unread | Self::NotUtf8 => None,
        }
    }
}

/// Why `admit` wrote no outputs (exit 2). `L`, `R`, `B`, `C`, `F` and `H` are the ledger's, the
/// refs', the bundle store's, the comments', GitHub's and the fetch's errors.
#[derive(Debug)]
pub enum AdmitRunError<L, R, B, C, F, H> {
    /// The plan artifact's `run.json` does not hash to plan's `run_digest`; nothing was read and
    /// no slot taken.
    PlanDigest,
    /// The plan's document was refused; nothing was read.
    Plan(PlanDocument),
    /// A pinned commit could not be fetched.
    Fetch {
        /// Which one.
        side: FetchedSide,
        /// Why.
        source: H,
    },
    /// The base P10 identity lists were refused or could not be read.
    Identities(BaseIdentitiesError<ForgeObjectsError>),
    /// Base `.assay/suppress.toml` was refused or could not be read.
    Suppress(SuppressError),
    /// The PR's push records could not be read.
    Pushes(LedgerReadError<L>),
    /// The PR's range could not be attributed.
    Range(RangeError<ForgeObjectsError, F>),
    /// The PR's ledger state could not be read.
    State(LedgerReadError<L>),
    /// The previous completed run's head could not be read from its bundle.
    PriorHead(PriorCarryError<B>),
    /// How the previous head stands to head could not be read.
    Target(TargetError<ForgeObjectsError>),
    /// The range-diff's git process group may still be running.
    RangeDiffKilled,
    /// The admission failed; [`AdmitError::AfterSlot`] says what became of a slot it took.
    Admit(Box<AdmitError<L, R, B, C, Infallible>>),
    /// `--out` or a document in it could not be written.
    Write {
        /// The directory or file.
        path: PathBuf,
        /// The failure.
        source: io::Error,
        /// The slot the run holds, for a spend: the job still reports it, so release releases it.
        held: Option<HeldSlot>,
    },
}

impl<L, R, B, C, F, H> AdmitRunError<L, R, B, C, F, H> {
    /// The slot this failure leaves the run holding, which the job still reports so `release`
    /// releases it: a [`Self::Write`] once a slot was taken, and every
    /// [`AdmitError::AfterSlot`], whether or not the admission could release the slot itself
    /// (`release` settles the reservation and finds a released slot reaped).
    #[must_use]
    pub fn held(&self) -> Option<HeldSlot> {
        match self {
            // The error keeps its slot for its message; the caller writes its own copy.
            Self::Write { held, .. } => held.clone(),
            Self::Admit(error) => match error.as_ref() {
                AdmitError::AfterSlot(after) => Some(HeldSlot::from(&after.hold)),
                // Every other admission failure comes before a slot is taken.
                _ => None,
            },
            Self::PlanDigest
            | Self::Plan(_)
            | Self::Fetch { .. }
            | Self::Identities(_)
            | Self::Suppress(_)
            | Self::Pushes(_)
            | Self::Range(_)
            | Self::State(_)
            | Self::PriorHead(_)
            | Self::Target(_)
            | Self::RangeDiffKilled => None,
        }
    }
}

impl<L, R, B, C, F, H> From<PriorError<L, B>> for AdmitRunError<L, R, B, C, F, H> {
    /// The previous run's reading failure, as admit reports it.
    fn from(error: PriorError<L, B>) -> Self {
        match error {
            PriorError::State(source) => Self::State(source),
            PriorError::PriorHead(source) => Self::PriorHead(source),
            PriorError::Target(source) => Self::Target(source),
            PriorError::RangeDiffKilled => Self::RangeDiffKilled,
        }
    }
}

/// The [`AdmitRunError`] of `run_admit` over the ports' error types.
pub type AdmitRunFailure<L, R, B, C, F, H> = AdmitRunError<
    <L as assay_app::Ledger>::Error,
    <R as LedgerRefs>::Error,
    <B as BundleSource>::Error,
    <C as CommandComments>::Error,
    <F as Forge>::Error,
    <H as FetchCommits>::Error,
>;

impl<L, R, B, C, F, H> fmt::Display for AdmitRunError<L, R, B, C, F, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlanDigest => {
                f.write_str("the plan artifact's run.json is not plan's run_digest")
            }
            Self::Plan(_) => f.write_str("the plan's run description is refused"),
            Self::Fetch { side, .. } => match side {
                FetchedSide::Base => f.write_str("could not fetch the base commit"),
                FetchedSide::Head => f.write_str("could not fetch the head commit"),
            },
            Self::Identities(_) => f.write_str("could not read the base identity lists"),
            Self::Suppress(_) => f.write_str("could not read the base suppress rules"),
            Self::Pushes(_) => f.write_str("could not read the PR's push records"),
            Self::Range(_) => f.write_str("could not attribute the PR's range"),
            Self::State(_) => f.write_str("could not read the PR's ledger state"),
            Self::PriorHead(_) => f.write_str("could not read the previous run's head"),
            Self::Target(_) => f.write_str("could not read how the previous head stands to head"),
            Self::RangeDiffKilled => {
                f.write_str("the range-diff's git process group may still be running")
            }
            Self::Admit(_) => f.write_str("the run could not be admitted"),
            Self::Write { path, .. } => write!(f, "cannot write {}", path.display()),
        }
    }
}

impl<L, R, B, C, F, H> Error for AdmitRunError<L, R, B, C, F, H>
where
    L: Error + 'static,
    R: Error + 'static,
    B: Error + 'static,
    C: Error + 'static,
    F: Error + 'static,
    H: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PlanDigest | Self::RangeDiffKilled => None,
            Self::Plan(source) => Some(source),
            Self::Fetch { source, .. } => Some(source),
            Self::Identities(source) => Some(source),
            Self::Suppress(source) => Some(source),
            Self::Pushes(source) | Self::State(source) => Some(source),
            Self::Range(source) => Some(source),
            Self::PriorHead(source) => Some(source),
            Self::Target(source) => Some(source),
            Self::Admit(source) => Some(source.as_ref()),
            Self::Write { source, .. } => Some(source),
        }
    }
}
