//! `assay decide-only publish` (#61 DO-12, D2 A, D11, J5, J10): what `decide-only decide` wrote
//! into its `--out`, published after the redecide job's `upload-artifact` step.
//!
//! [`run_republish`] reads [`STATE_FILE`] from decide's `--out` as a [`RedecideState`] (a
//! directory and a regular file, neither reached through a symlink, at most
//! [`MAX_STATE_BYTES`]). A silent state publishes nothing and calls no port. For a decided
//! state, the bundle, [`DECISION_FILE`] beside it (at most [`MAX_DECISION_BUNDLE_BYTES`]), must
//! hash to the state's digest before any port is called. Then the PR's live head and base, the
//! default branch, and the PR's ledger state, from which decide-only's [`Claim`] is taken again
//! by the rule decide applied (the register of the live head, written from the state's own
//! evidence or decision): when there is none, the check is no longer decide-only's and nothing
//! is published ([`RepublishRun::Unclaimed`]). A refusal is published over the claim through
//! [`publish_redecide_refusal`]; a decision through [`publish_redecided`], which confirms the
//! upload by sha256 only after the upload step reported `success`, writes the decision pointer
//! and the register under the lock (J10), and otherwise fails a trust-path or exec-surface PR
//! with `persist-error`.

mod error;

use std::convert::Infallible;
use std::num::NonZeroU64;
use std::path::Path;

use assay_app::{
    DECISION_FILE, DecisionBundleSource, Fetched, Ledger, MAX_DECISION_BUNDLE_BYTES,
    PersistOutcome, PublishLedger, Published, PullRequestState, RedecideRefusal, RedecideTarget,
    RedecidedPersisted, RedecidedPorts, Sha256Digest, Unconfirmed, UnusableEvidence, VerdictForge,
    publish_redecide_refusal, publish_redecided, read_state,
};
use assay_domain::Rubric;

pub use self::error::{RepublishRunError, RepublishRunFailure};
use crate::artifacts::read_in;
use crate::ci::FirstAttempt;
use crate::decide::{MAX_STATE_BYTES, STATE_FILE};
use crate::outputs::DecideOnlyGuardOutputs;
use crate::redecide::{DefaultBranch, RedecideState, claim};

/// What `decide-only publish` is given besides its ports.
#[derive(Debug)]
pub struct RepublishJob<'a> {
    /// The run, checked to be its first attempt (J2).
    pub run: &'a FirstAttempt,
    /// What guard wrote: the PRs this run decides.
    pub guard: &'a DecideOnlyGuardOutputs,
    /// `ASSAY_PR`: the matrix entry this job publishes for.
    pub pr: NonZeroU64,
    /// rubric v1, as the binary embeds it.
    pub rubric: &'a Rubric,
    /// How the upload step ended (`ASSAY_PERSIST`).
    pub upload: PersistOutcome,
}

/// What `decide-only publish` reads and writes through, borrowed for one call.
#[derive(Debug)]
pub struct RepublishJobPorts<'p, A, R, L, P> {
    /// GitHub with the `assay-verdict` App's token: the live PR, the verdict check and the
    /// summary comment.
    pub app: &'p A,
    /// GitHub with the job's run token: the default branch and the uploaded decision bundle.
    pub run: &'p R,
    /// The ledger's files: the PR's state, whose register and evidence pointer are the claim.
    pub ledger: &'p L,
    /// The per-head lock, the decision pointer and the register.
    pub lock: &'p P,
}

/// What `decide-only publish` published. `P` and `B` are the publish ledger's and the
/// decision bundle store's errors.
#[derive(Debug)]
pub enum RepublishRun<P, B> {
    /// Nothing: decide found nothing to publish.
    Silent,
    /// Nothing: the ledger's register is no longer one decide-only may write over for the PR's
    /// live head (another run published, or the PR moved), so the check is not decide-only's.
    Unclaimed,
    /// The refusal check over the claim, or nothing where the check is no longer claimed.
    Refused(Published<P, Infallible>),
    /// The persisted re-decision.
    Decided(Published<P, Unconfirmed<B>>),
}

/// What [`run_republish`] returns over the ports' types.
pub type RepublishRunOutcome<A, R, L, P> = Result<
    RepublishRun<<P as PublishLedger>::Error, <R as DecisionBundleSource>::Error>,
    RepublishRunFailure<A, L, P>,
>;

/// What decide's `--out` holds to publish, read and checked before any port is called.
enum Pending {
    /// A refusal of the evidence.
    Refused(RedecideRefusal),
    /// A re-decision: the bundle's bytes, which hash to `digest`.
    Decided {
        bundle: Vec<u8>,
        digest: Sha256Digest,
    },
}

/// Publishes what `decide-only decide` wrote into `state` for `job`'s PR, through `ports`.
///
/// # Errors
///
/// [`RepublishRunError`] when `job.pr` is not one of guard's, or decide's state or bundle
/// cannot be read, is refused or does not hash to the state's digest, all before any port is
/// called; when the PR, the default branch or the ledger state cannot be read; or when the
/// verdict cannot be published.
pub fn run_republish<A, R, L, P, E>(
    job: &RepublishJob<'_>,
    ports: &RepublishJobPorts<'_, A, R, L, P>,
    state: &Path,
) -> RepublishRunOutcome<A, R, L, P>
where
    A: VerdictForge<Error = E> + PullRequestState<Error = E>,
    R: DecisionBundleSource + DefaultBranch<Error = E>,
    L: Ledger,
    P: PublishLedger,
{
    if !job.guard.prs().as_slice().contains(&job.pr) {
        return Err(RepublishRunError::NotScheduled { pr: job.pr });
    }
    let Some(pending) = read_out(state)? else {
        return Ok(RepublishRun::Silent);
    };
    let live = ports.app.current(job.pr).map_err(RepublishRunError::Live)?;
    let default_branch = ports
        .run
        .default_branch()
        .map_err(RepublishRunError::DefaultBranch)?;
    let run_uid = job.run.run_env().run_uid;
    let pr_state =
        read_state(ports.ledger, run_uid.repo_id, job.pr).map_err(RepublishRunError::Ledger)?;
    let Some(claim) = claim(pr_state, run_uid.repo_id, &live.head_sha) else {
        return Ok(RepublishRun::Unclaimed);
    };
    let target = RedecideTarget {
        run_uid,
        number: job.pr,
        head_sha: &live.head_sha,
        base_ref: &live.base_ref,
        default_branch: &default_branch,
    };
    match pending {
        Pending::Refused(refusal) => {
            let unusable = UnusableEvidence {
                target,
                claim: &claim,
                refusal,
            };
            publish_redecide_refusal(&unusable, ports.app, ports.lock)
                .map(RepublishRun::Refused)
                .map_err(RepublishRunError::Refusal)
        }
        Pending::Decided { bundle, digest } => {
            let persisted = RedecidedPersisted {
                target,
                claim: &claim,
                rubric: job.rubric,
                bundle: &bundle,
                digest: &digest,
                upload: job.upload,
            };
            let redecided_ports = RedecidedPorts {
                forge: ports.app,
                ledger: ports.lock,
                bundles: ports.run,
            };
            publish_redecided(&persisted, &redecided_ports)
                .map(RepublishRun::Decided)
                .map_err(RepublishRunError::Decided)
        }
    }
}

/// What decide's state in `dir` asks to publish, `None` for a silent one; for a decision, the
/// bundle beside it, which must hash to the state's digest.
fn read_out<G, L, P>(dir: &Path) -> Result<Option<Pending>, RepublishRunError<G, L, P>> {
    let state = RedecideState::read(&read(dir, STATE_FILE, MAX_STATE_BYTES)?)
        .map_err(RepublishRunError::State)?;
    match state {
        RedecideState::Silent => Ok(None),
        RedecideState::Refused(refusal) => Ok(Some(Pending::Refused(refusal))),
        RedecideState::Decided { digest } => {
            let bundle = read(dir, DECISION_FILE, MAX_DECISION_BUNDLE_BYTES)?;
            if Sha256Digest::of(&bundle) == digest {
                Ok(Some(Pending::Decided { bundle, digest }))
            } else {
                Err(RepublishRunError::Digest)
            }
        }
    }
}

/// File `file` of decide's `--out` `dir`, which must be there and within `limit` bytes.
fn read<G, L, P>(
    dir: &Path,
    file: &str,
    limit: usize,
) -> Result<Vec<u8>, RepublishRunError<G, L, P>> {
    match read_in(dir, file, limit).map_err(RepublishRunError::Read)? {
        Fetched::Found(bytes) => Ok(bytes),
        Fetched::Missing => Err(RepublishRunError::Missing {
            path: dir.join(file),
        }),
        Fetched::TooLarge => Err(RepublishRunError::TooLarge {
            path: dir.join(file),
        }),
    }
}
