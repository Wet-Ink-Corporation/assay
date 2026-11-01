//! Why `run_redecide` wrote no outputs (exit 2): nothing is published for the PR by this run.

use std::error::Error;
use std::fmt;
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_app::{
    BundleSource, CommandTimeError, EvidenceError, InputsError, Ledger, LedgerReadError,
    LedgerSnapshotError, LedgerWriteError, ListsError, PrepareDecisionError, PullRequestState,
    RedecideStateError, TipLists,
};

/// How the triggering event disagrees with what guard said it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PayloadMismatch {
    /// The payload is for another repository than the run's.
    Repository,
    /// The payload is of another event than guard's `trigger`.
    Trigger,
    /// The payload is about another PR than `ASSAY_PR`.
    PullRequest,
    /// A label event's head is not in this repository (0006; guard refused it already).
    Head,
}

impl PayloadMismatch {
    /// What the payload disagrees about.
    const fn what(self) -> &'static str {
        match self {
            Self::Repository => "its repository is not the run's",
            Self::Trigger => "it is not the event guard reported",
            Self::PullRequest => "its PR is not ASSAY_PR",
            Self::Head => "its head is not in this repository",
        }
    }
}

/// Why `decide-only decide` wrote no outputs.
///
/// `G` is GitHub's error (the live PR, reviews, comments, replies, provenance and the default
/// branch), `T` the tip lists', `B` the artifact store's and `L` the ledger's.
#[derive(Debug)]
pub enum RedecideRunError<G, T, B, L> {
    /// `ASSAY_PR` is not one of the PRs guard wrote in `prs`.
    NotScheduled {
        /// `ASSAY_PR`.
        pr: NonZeroU64,
    },
    /// The event payload does not match guard's outputs.
    Payload(PayloadMismatch),
    /// The PR's live head and base could not be read.
    Live(G),
    /// The base tip's eligibility lists could not be read.
    TipLists(T),
    /// A base tip list file is not UTF-8.
    TipListText {
        /// The file: [`OWNERS_PATH`](assay_app::OWNERS_PATH) or
        /// [`AGENTS_PATH`](assay_app::AGENTS_PATH).
        file: &'static str,
    },
    /// The base tip's eligibility lists were refused.
    Lists(ListsError),
    /// The command could not be counted (J7).
    Count(LedgerWriteError<L>),
    /// The PR's ledger state could not be read.
    State(LedgerReadError<L>),
    /// The repository's default branch could not be read.
    DefaultBranch(G),
    /// The evidence could not be read; a verdict about it is a state, not this.
    Evidence(EvidenceError<G, B>),
    /// The re-decision snapshot could not be taken.
    Snapshot(Box<RedecideStateError<G, LedgerSnapshotError<L>, G>>),
    /// The command's comment time cannot be recorded.
    Record(CommandTimeError),
    /// The command record could not be appended; the comment was answered that it has no
    /// effect, unless `reply` says that failed too.
    Append {
        /// The ledger's failure.
        source: LedgerWriteError<L>,
        /// The reply's failure, if it failed.
        reply: Option<G>,
    },
    /// A reply could not be posted.
    Reply(G),
    /// The deferral record's flag could not be set (D14).
    Deferral(LedgerWriteError<L>),
    /// The decision inputs could not be composed.
    Inputs(InputsError),
    /// The decision bundle could not be built.
    Prepare(PrepareDecisionError),
    /// `--out` or a file in it could not be written.
    Write {
        /// The directory or file.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
}

/// The [`RedecideRunError`] of `run_redecide` over its ports.
pub type RedecideRunFailure<A, R, L> = RedecideRunError<
    <A as PullRequestState>::Error,
    <A as TipLists>::Error,
    <R as BundleSource>::Error,
    <L as Ledger>::Error,
>;

impl<G, T, B, L> fmt::Display for RedecideRunError<G, T, B, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotScheduled { pr } => write!(f, "ASSAY_PR {pr} is not one of guard's prs"),
            Self::Payload(mismatch) => {
                write!(f, "the event payload is refused: {}", mismatch.what())
            }
            Self::Live(_) => f.write_str("could not read the PR's live head and base"),
            Self::TipLists(_) => f.write_str("could not read the base tip's eligibility lists"),
            Self::TipListText { file } => write!(f, "the base tip's {file} is not UTF-8"),
            Self::Lists(_) => f.write_str("the base tip's eligibility lists were refused"),
            Self::Count(_) => f.write_str("could not count the command in the PR's ledger state"),
            Self::State(_) => f.write_str("could not read the PR's ledger state"),
            Self::DefaultBranch(_) => f.write_str("could not read the default branch"),
            Self::Evidence(_) => f.write_str("could not read the evidence bundle"),
            Self::Snapshot(_) => f.write_str("could not take the re-decision snapshot"),
            Self::Record(_) => f.write_str("the command cannot be recorded"),
            Self::Append { reply, .. } => match reply {
                None => f.write_str("could not record the command; its comment was answered"),
                Some(_) => f.write_str(
                    "could not record the command, nor answer its comment that it has no effect",
                ),
            },
            Self::Reply(_) => f.write_str("could not reply to the command"),
            Self::Deferral(_) => f.write_str("could not flag the PR's deferral record"),
            Self::Inputs(_) => f.write_str("could not compose the decision inputs"),
            Self::Prepare(_) => f.write_str("could not build the decision bundle"),
            Self::Write { path, .. } => write!(f, "cannot write {}", path.display()),
        }
    }
}

impl<G, T, B, L> Error for RedecideRunError<G, T, B, L>
where
    G: Error + 'static,
    T: Error + 'static,
    B: Error + 'static,
    L: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotScheduled { .. } | Self::Payload(_) | Self::TipListText { .. } => None,
            Self::Live(source) | Self::DefaultBranch(source) | Self::Reply(source) => Some(source),
            Self::TipLists(source) => Some(source),
            Self::Lists(source) => Some(source),
            Self::Count(source) | Self::Deferral(source) | Self::Append { source, .. } => {
                Some(source)
            }
            Self::State(source) => Some(source),
            Self::Evidence(source) => Some(source),
            Self::Snapshot(source) => Some(source.as_ref()),
            Self::Record(source) => Some(source),
            Self::Inputs(source) => Some(source),
            Self::Prepare(source) => Some(source),
            Self::Write { source, .. } => Some(source),
        }
    }
}
