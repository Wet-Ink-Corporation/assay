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
//! is published ([`RepublishRun::Unclaimed`]). The claim may have moved since decide read it (a
//! pipeline run of the same head published new evidence), so each outcome is checked against
//! it first, and is otherwise unclaimed too (wi-b70b90): a refusal is published only when the
//! claim's evidence, read again through [`read_evidence`] as decide read it, still gives the
//! same refusal, and then through [`publish_redecide_refusal`]; a decision only when its bundle
//! names the claim's evidence pointer and the live head, and then through
//! [`publish_redecided`], which confirms the upload by sha256 only after the upload step
//! reported `success`, writes the decision pointer and the register under the lock (J10), and
//! otherwise fails a trust-path or exec-surface PR with `persist-error`.

mod error;

use std::convert::Infallible;
use std::num::NonZeroU64;
use std::path::Path;

use assay_app::{
    BundleSource, Claim, DECISION_FILE, DecisionBundleSource, EvidenceRead, EvidenceRequest,
    Fetched, Ledger, MAX_DECISION_BUNDLE_BYTES, PersistOutcome, PublishLedger, Published,
    PullRequestState, RedecideRefusal, RedecideTarget, RedecidedError, RedecidedPersisted,
    RedecidedPorts, RedecidedRefused, RunProvenance, Sha256Digest, Unconfirmed, UnusableEvidence,
    VerdictForge, publish_redecide_refusal, publish_redecided, read_decision_bundle, read_evidence,
    read_state,
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
    /// GitHub with the job's run token: the default branch, the uploaded decision bundle, and
    /// the evidence run's provenance and bundle, read again for a refusal.
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
    /// live head (another run published, or the PR moved), or the claim it gives is not the one
    /// decide's outcome was reached over (other evidence, which no longer gives the refusal, or
    /// a bundle of another pointer or head), so the check is not this run's.
    Unclaimed,
    /// The refusal check over the claim, or nothing where the check is no longer claimed.
    Refused(Published<P, Infallible>),
    /// The persisted re-decision.
    Decided(Published<P, Unconfirmed<B>>),
}

/// What [`run_republish`] returns over the ports' types.
pub type RepublishRunOutcome<A, R, L, P> = Result<
    RepublishRun<<P as PublishLedger>::Error, <R as DecisionBundleSource>::Error>,
    RepublishRunFailure<A, R, L, P>,
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
/// called; when the PR, the default branch, the ledger state or (for a refusal) the claim's
/// evidence cannot be read; when the decision bundle is unreadable; or when the verdict cannot
/// be published.
pub fn run_republish<A, R, L, P, E>(
    job: &RepublishJob<'_>,
    ports: &RepublishJobPorts<'_, A, R, L, P>,
    state: &Path,
) -> RepublishRunOutcome<A, R, L, P>
where
    A: VerdictForge<Error = E> + PullRequestState<Error = E>,
    R: DecisionBundleSource + DefaultBranch<Error = E> + RunProvenance<Error = E> + BundleSource,
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
    let claimed = Claimed {
        target: RedecideTarget {
            run_uid,
            number: job.pr,
            head_sha: &live.head_sha,
            base_ref: &live.base_ref,
            default_branch: &default_branch,
        },
        claim: &claim,
    };
    match pending {
        Pending::Refused(refusal) => publish_refusal(job, ports, &claimed, refusal),
        Pending::Decided { bundle, digest } => {
            publish_decision(job, ports, &claimed, (&bundle, &digest))
        }
    }
}

/// Where decide-only publishes, and the claim it re-took from the ledger for the live head.
struct Claimed<'a> {
    target: RedecideTarget<'a>,
    claim: &'a Claim,
}

/// Publishes `refusal` over the claim, only while the evidence the claim points at, read again
/// as decide read it, still gives that refusal: a pipeline run that published new evidence
/// between the two steps is not refused with the old evidence's verdict (wi-b70b90).
fn publish_refusal<A, R, L, P, E>(
    job: &RepublishJob<'_>,
    ports: &RepublishJobPorts<'_, A, R, L, P>,
    claimed: &Claimed<'_>,
    refusal: RedecideRefusal,
) -> RepublishRunOutcome<A, R, L, P>
where
    A: VerdictForge<Error = E>,
    R: DecisionBundleSource + RunProvenance<Error = E> + BundleSource,
    L: Ledger,
    P: PublishLedger,
{
    let request = EvidenceRequest {
        repo_id: claimed.target.run_uid.repo_id,
        pointer: &claimed.claim.evidence,
        default_branch: claimed.target.default_branch,
        rubric: job.rubric,
    };
    let read =
        read_evidence(&request, ports.run, ports.run).map_err(RepublishRunError::Evidence)?;
    let still = match read {
        EvidenceRead::Forged(_) => Some(RedecideRefusal::Forged),
        EvidenceRead::Absent(absence) => Some(RedecideRefusal::Absent(absence)),
        EvidenceRead::Authentic(_) | EvidenceRead::OtherRubric => None,
    };
    if still != Some(refusal) {
        return Ok(RepublishRun::Unclaimed);
    }
    let unusable = UnusableEvidence {
        target: claimed.target,
        claim: claimed.claim,
        refusal,
    };
    publish_redecide_refusal(&unusable, ports.app, ports.lock)
        .map(RepublishRun::Refused)
        .map_err(RepublishRunError::Refusal)
}

/// Publishes the re-decision `bundle` (hashing to `digest`) over the claim, only when it was
/// decided for the claim's evidence pointer and the live head; otherwise the check is no longer
/// this decision's to write.
fn publish_decision<A, R, L, P, E>(
    job: &RepublishJob<'_>,
    ports: &RepublishJobPorts<'_, A, R, L, P>,
    claimed: &Claimed<'_>,
    (bundle, digest): (&[u8], &Sha256Digest),
) -> RepublishRunOutcome<A, R, L, P>
where
    A: VerdictForge<Error = E>,
    R: DecisionBundleSource + BundleSource,
    L: Ledger,
    P: PublishLedger,
{
    let decided = read_decision_bundle(bundle).map_err(|error| {
        RepublishRunError::Decided(RedecidedError::Bundle(RedecidedRefused::Unreadable(error)))
    })?;
    if decided.evidence() != &claimed.claim.evidence || decided.head() != claimed.target.head_sha {
        return Ok(RepublishRun::Unclaimed);
    }
    let persisted = RedecidedPersisted {
        target: claimed.target,
        claim: claimed.claim,
        rubric: job.rubric,
        bundle,
        digest,
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

/// What decide's state in `dir` asks to publish, `None` for a silent one; for a decision, the
/// bundle beside it, which must hash to the state's digest.
fn read_out<G, B, L, P>(dir: &Path) -> Result<Option<Pending>, RepublishRunError<G, B, L, P>> {
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
fn read<G, B, L, P>(
    dir: &Path,
    file: &str,
    limit: usize,
) -> Result<Vec<u8>, RepublishRunError<G, B, L, P>> {
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
