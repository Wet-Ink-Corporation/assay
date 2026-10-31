//! The pipeline's `publish` step (#60 ST-14, Z4, D5 A): what `assay decide` wrote into its
//! `--out`, published after the workflow's `upload-artifact` step.
//!
//! [`run_publish`] reads [`STATE_FILE`] from decide's `--out` (a directory and a regular file,
//! neither reached through a symlink, at most [`MAX_STATE_BYTES`]). A silent run publishes
//! nothing and calls no port (R3-06). A refusal publishes `failure` with the refusal's reason
//! through [`assay_app::publish_refusal`] and persists nothing. A decided run's bundle,
//! [`BUNDLE_FILE`] beside the state (at most [`MAX_BUNDLE_BYTES`]), is published through
//! [`assay_app::publish_persisted`], which refuses bytes that do not hash to the state's
//! digest, confirms the upload through the artifact store by sha256 only after the upload
//! step reported `success`, and otherwise fails a trust-path or exec-surface PR with
//! `persist-error` (S9).

use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use assay_app::{
    BUNDLE_FILE, BundleSource, Fetched, MAX_BUNDLE_BYTES, PersistOutcome, Persisted,
    PersistedError, PersistedPorts, PublishError, PublishLedger, Published, Target, Unconfirmed,
    VerdictForge, publish_persisted, publish_refusal,
};
use assay_domain::{Aeu, Rubric};

use crate::artifacts::{DirArtifactsError, read_in};
use crate::ci::FirstAttempt;
use crate::decide::{DecideState, DecideStateError, MAX_STATE_BYTES, STATE_FILE};
use crate::outputs::Identity;
use crate::plan::pull_request_event;

/// What `publish` is given besides its ports.
#[derive(Debug)]
pub struct PublishJob<'a> {
    /// The run, checked to be its first attempt.
    pub run: &'a FirstAttempt,
    /// guard's identity of the PR, as `decide` read it.
    pub identity: &'a Identity,
    /// What the run's LLM stage cost, as `decide` was given it; shown in the summary only.
    pub cost: Aeu,
    /// Rubric v1.
    pub rubric: &'a Rubric,
    /// How the upload step ended (`ASSAY_PERSIST`).
    pub upload: PersistOutcome,
}

/// What `publish` writes through, borrowed for one call.
#[derive(Debug)]
pub struct PublishJobPorts<'p, F, L, B> {
    /// The verdict check and the summary comment.
    pub forge: &'p F,
    /// The per-head lock and the evidence pointer.
    pub ledger: &'p L,
    /// The run's uploaded artifacts.
    pub bundles: &'p B,
}

/// What `publish` published. `L` and `B` are the ledger's and the artifact store's errors.
#[derive(Debug)]
pub enum PublishRun<L, B> {
    /// Nothing: the run was superseded or yields to a newer one.
    Silent,
    /// The refusal check, or nothing where the check is no longer this run's.
    Refused(Published<L, Infallible>),
    /// The persisted decision.
    Decided(Published<L, Unconfirmed<B>>),
}

/// Why `publish` published nothing (exit 2): the check `open` left `in_progress` is closed as
/// `failure publish-error` by the reconcile (06 §4). `F` and `L` are GitHub's and the ledger's
/// errors.
#[derive(Debug)]
pub enum PublishRunError<F, L> {
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
    State(DecideStateError),
    /// The refusal could not be published.
    Refusal(PublishError<F, L>),
    /// The decision could not be published, or its bundle is not this run's decision.
    Persisted(PersistedError<F, L>),
}

/// The [`PublishRunError`] of [`run_publish`] over the ports' error types.
pub type PublishRunFailure<F, L> =
    PublishRunError<<F as VerdictForge>::Error, <L as PublishLedger>::Error>;

/// What [`run_publish`] returns over the ports' error types.
pub type PublishRunOutcome<F, L, B> = Result<
    PublishRun<<L as PublishLedger>::Error, <B as BundleSource>::Error>,
    PublishRunFailure<F, L>,
>;

impl<F, L> fmt::Display for PublishRunError<F, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "{} is not there", path.display()),
            Self::TooLarge { path } => write!(f, "{} is over its size cap", path.display()),
            Self::Read(_) => f.write_str("could not read decide's output"),
            Self::State(_) => f.write_str("decide's state was refused"),
            Self::Refusal(_) => f.write_str("could not publish the refusal"),
            Self::Persisted(_) => f.write_str("could not publish the decision"),
        }
    }
}

impl<F, L> Error for PublishRunError<F, L>
where
    F: Error + 'static,
    L: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Missing { .. } | Self::TooLarge { .. } => None,
            Self::Read(source) => Some(source),
            Self::State(source) => Some(source),
            Self::Refusal(source) => Some(source),
            Self::Persisted(source) => Some(source),
        }
    }
}

/// Publishes what `decide` wrote into `state` for `job`'s run, through `ports`.
///
/// # Errors
///
/// [`PublishRunError`] when decide's state or bundle cannot be read or is refused, before any
/// port is called, or when the verdict cannot be published.
pub fn run_publish<F, L, B>(
    job: &PublishJob<'_>,
    ports: &PublishJobPorts<'_, F, L, B>,
    state: &Path,
) -> PublishRunOutcome<F, L, B>
where
    F: VerdictForge,
    L: PublishLedger,
    B: BundleSource,
{
    let decided = DecideState::read(&read(state, STATE_FILE, MAX_STATE_BYTES)?)
        .map_err(PublishRunError::State)?;
    let run_env = job.run.run_env();
    // The event owns its copy of the PR's fields; the identity keeps the default branch.
    let pull_request = pull_request_event(run_env, job.identity.clone());
    let target = Target {
        run_uid: run_env.run_uid,
        pull_request: &pull_request,
        default_branch: &job.identity.default_branch,
    };
    match decided {
        DecideState::Silent => Ok(PublishRun::Silent),
        DecideState::Refused(refusal) => {
            publish_refusal(&target, &refusal, ports.forge, ports.ledger)
                .map(PublishRun::Refused)
                .map_err(PublishRunError::Refusal)
        }
        DecideState::Decided { digest } => {
            let bundle = read(state, BUNDLE_FILE, MAX_BUNDLE_BYTES)?;
            let persisted = Persisted {
                target,
                rubric: job.rubric,
                cost: job.cost,
                bundle: &bundle,
                digest: &digest,
                upload: job.upload,
            };
            let persisted_ports = PersistedPorts {
                forge: ports.forge,
                ledger: ports.ledger,
                bundles: ports.bundles,
            };
            publish_persisted(&persisted, &persisted_ports)
                .map(PublishRun::Decided)
                .map_err(PublishRunError::Persisted)
        }
    }
}

/// File `file` of decide's `--out` `dir`, which must be there and within `limit` bytes.
fn read<F, L>(dir: &Path, file: &str, limit: usize) -> Result<Vec<u8>, PublishRunError<F, L>> {
    match read_in(dir, file, limit).map_err(PublishRunError::Read)? {
        Fetched::Found(bytes) => Ok(bytes),
        Fetched::Missing => Err(PublishRunError::Missing {
            path: dir.join(file),
        }),
        Fetched::TooLarge => Err(PublishRunError::TooLarge {
            path: dir.join(file),
        }),
    }
}
