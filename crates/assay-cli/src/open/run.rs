//! [`run_open`]: `assay open` over any ports. It reads what the event does not carry (the
//! run's `created_at`, and for a `synchronize` whether the push was forced), builds the
//! [`OpenRequest`] from `guard`'s identity and runs [`assay_app::open`].

use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;

use assay_app::{
    CommitId, Forge, Ledger, LedgerRefs, LedgerTime, OpenClock, OpenFailure, OpenPorts,
    OpenRequest, OpenTrigger, Opened, PushEvent, open,
};
use assay_forge::github::{ForgeError, GitHub, Sleep, Transport};

use crate::ci::FirstAttempt;
use crate::outputs::{Identity, Trigger};

/// The GitHub reads `open` makes before it writes anything, besides the app's ports.
pub trait OpenReads {
    /// Why a read failed.
    type Error: Error + 'static;

    /// When workflow run `run_id` was created: the push record's `at` (04 §5).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when it cannot be read exactly.
    fn run_created_at(&self, run_id: NonZeroU64) -> Result<LedgerTime, Self::Error>;

    /// Whether moving the head from `before` to `after` rewrote history.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when it cannot be read exactly.
    fn forced(&self, before: &CommitId, after: &CommitId) -> Result<bool, Self::Error>;
}

impl<T: Transport, S: Sleep> OpenReads for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn run_created_at(&self, run_id: NonZeroU64) -> Result<LedgerTime, Self::Error> {
        Self::run_created_at(self, run_id)
    }

    fn forced(&self, before: &CommitId, after: &CommitId) -> Result<bool, Self::Error> {
        Self::forced(self, before, after)
    }
}

/// What one `assay open` runs for: the run, shown to be a first attempt, and the identity
/// `guard` resolved (I2).
#[derive(Debug, Clone, Copy)]
pub struct OpenJob<'a> {
    /// The run.
    pub run: &'a FirstAttempt,
    /// The PR, its head and base, and the event behind the run.
    pub identity: &'a Identity,
}

/// Why [`run_open`] stopped. Nothing is written to the ledger or the check before a read
/// fails.
#[derive(Debug)]
pub enum OpenRunError<E, O> {
    /// The run's `created_at` could not be read.
    RunCreatedAt(E),
    /// Whether the push was forced could not be read.
    Forced(E),
    /// [`assay_app::open`] aborted.
    Open(O),
}

impl<E, O> fmt::Display for OpenRunError<E, O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RunCreatedAt(_) => "the workflow run's created_at could not be read",
            Self::Forced(_) => "whether the push rewrote history could not be read",
            Self::Open(_) => "the verdict check could not be opened",
        })
    }
}

impl<E: Error + 'static, O: Error + 'static> Error for OpenRunError<E, O> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RunCreatedAt(source) | Self::Forced(source) => Some(source),
            Self::Open(source) => Some(source),
        }
    }
}

/// What [`run_open`] returns over its ports' error types.
pub type OpenRunOutcome<G, F, L, R, C> = Result<
    Opened<<F as Forge>::Error, <R as LedgerRefs>::Error>,
    OpenRunError<<G as OpenReads>::Error, OpenFailure<F, L, R, C>>,
>;

/// Opens the verdict check for `job` (S0) through [`assay_app::open`].
///
/// It first reads the run's `created_at` (the record's `at`; `ports.clock` only times the
/// lock's lease) and, for a `synchronize`, whether the push was forced. An `opened` appends the
/// opener record (its sender), a `synchronize` the push record; any other event writes no
/// record.
///
/// # Errors
///
/// [`OpenRunError::RunCreatedAt`] and [`OpenRunError::Forced`] when a read fails, before any
/// write; [`OpenRunError::Open`] when the use case aborts.
pub fn run_open<G, F, L, R, C>(
    job: OpenJob<'_>,
    reads: &G,
    ports: &OpenPorts<'_, F, L, R, C>,
) -> OpenRunOutcome<G, F, L, R, C>
where
    G: OpenReads,
    F: Forge,
    L: Ledger,
    R: LedgerRefs,
    C: OpenClock,
{
    let run = job.run.run_env();
    let identity = job.identity;
    let run_created_at = reads
        .run_created_at(run.run_uid.run_id)
        .map_err(OpenRunError::RunCreatedAt)?;
    let trigger = match &identity.trigger {
        Trigger::Opened => OpenTrigger::Opened {
            sender: identity.sender,
        },
        Trigger::Synchronize { before } => OpenTrigger::Synchronize(PushEvent {
            forced: reads
                .forced(before, &identity.head_sha)
                .map_err(OpenRunError::Forced)?,
            // The event stores the head before the push; the identity keeps its own.
            before: before.clone(),
            sender: identity.sender,
        }),
        Trigger::Reopened => OpenTrigger::Reopened,
        Trigger::ReadyForReview => OpenTrigger::ReadyForReview,
        Trigger::Edited => OpenTrigger::Edited,
        Trigger::Dispatch => OpenTrigger::Dispatch,
    };
    let repo = run.repo.to_string();
    let request = OpenRequest {
        run_uid: run.run_uid,
        repo: &repo,
        pr: identity.pr,
        head: &identity.head_sha,
        base_ref: &identity.base_ref,
        default_branch: &identity.default_branch,
        trigger: &trigger,
        run_created_at,
    };
    open(&request, ports).map_err(OpenRunError::Open)
}
