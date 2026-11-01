//! decide-only's publication (#61, D2 A, J5, J10): the re-decision `prepare_decision` built and
//! the workflow uploaded, published only over the key the publication register names.
//!
//! [`publish_redecided`] re-reads the decision bundle strictly, recomputes its decision, confirms
//! the upload through [`DecisionBundleSource`] by sha256, and then, under the per-head lock and
//! while the check and the ledger still hold decide-only's [`Claim`], writes the decision
//! pointer with the register in one compare-and-set, then the check and the summary. An
//! unconfirmed upload writes no pointer and no register, and fails closed on trust-path and
//! exec-surface PRs (`persist-error`), as S9 does for the pipeline.

mod refusal;

use std::num::NonZeroU64;

use assay_domain::{Aeu, Rubric, RunUid};
use thiserror::Error;

use super::persisted::{PersistOutcome, Unconfirmed};
use super::ports::{Claim, PublishLedger, VerdictForge};
use super::write::{self, Basis, Claimant, Pending, Site, Writer};
use super::{PersistFailure, PublishError, Published};
use crate::config::{CommitId, Sha256Digest};
use crate::redecide::{
    DecisionBundle, DecisionBundleError, MAX_DECISION_BUNDLE_BYTES, read_decision_bundle,
};
use crate::replay::{DecisionBundleSource, DecisionDiff, StoredBundle};

pub use self::refusal::{RedecideRefusal, UnusableEvidence, publish_redecide_refusal};

/// The decide-only run that publishes, and the PR as it is now.
#[derive(Debug, Clone, Copy)]
pub struct RedecideTarget<'a> {
    /// decide-only's run (attempt 1: no other publishes).
    pub run_uid: RunUid,
    /// The PR number.
    pub number: NonZeroU64,
    /// The PR's live head, the one re-decided.
    pub head_sha: &'a CommitId,
    /// The PR's live base ref.
    pub base_ref: &'a str,
    /// The repository's default branch, which names the check (R3-03).
    pub default_branch: &'a str,
}

impl<'a> RedecideTarget<'a> {
    /// Whether the run is attempt 1, the only one that publishes (R3-05).
    const fn is_first_attempt(&self) -> bool {
        self.run_uid.run_attempt.get() == NonZeroU64::MIN.get()
    }

    /// Where the check is written.
    const fn site(&self) -> Site<'a> {
        Site {
            run_uid: self.run_uid,
            number: self.number,
            head_sha: self.head_sha,
            base_ref: self.base_ref,
            default_branch: self.default_branch,
        }
    }
}

/// A persisted re-decision to publish.
#[derive(Debug, Clone, Copy)]
pub struct RedecidedPersisted<'a> {
    /// The run and the PR it publishes for.
    pub target: RedecideTarget<'a>,
    /// What decide-only read of the PR's state: the register it writes over and the evidence
    /// pointer it re-decided from.
    pub claim: &'a Claim,
    /// This build's rubric, whose version the bundle must record.
    pub rubric: &'a Rubric,
    /// The bundle's bytes as [`prepare_decision`](crate::prepare_decision) built them.
    pub bundle: &'a [u8],
    /// Their sha256 as the decide step published it.
    pub digest: &'a Sha256Digest,
    /// How the upload step ended.
    pub upload: PersistOutcome,
}

/// The ports [`publish_redecided`] uses, borrowed for one call.
#[derive(Debug)]
pub struct RedecidedPorts<'p, F, L, S> {
    /// The verdict check and the summary comment.
    pub forge: &'p F,
    /// The per-head lock and the PR's ledger state.
    pub ledger: &'p L,
    /// decide-only's uploaded decision bundles.
    pub bundles: &'p S,
}

/// Why a bundle handed to [`publish_redecided`] is not the target's re-decision. Nothing is read
/// from or written to any port, and nothing from the bundle is echoed (wi-b5affa).
#[derive(Debug, Error)]
pub enum RedecidedRefused {
    /// The bundle is larger than [`MAX_DECISION_BUNDLE_BYTES`].
    #[error("the decision bundle is larger than {MAX_DECISION_BUNDLE_BYTES} bytes")]
    TooLarge,
    /// The bytes do not hash to the digest given with them.
    #[error("the decision bundle does not hash to its digest")]
    Digest,
    /// The target is a re-run: only attempt 1 publishes (R3-05).
    #[error("only attempt 1 publishes")]
    NotFirstAttempt,
    /// The bytes are not `assay.run-decision/1` as decide-only writes it.
    #[error("the decision bundle is unreadable: {0}")]
    Unreadable(DecisionBundleError),
    /// The bundle records another run than the target's.
    #[error("the decision bundle records another run")]
    Run,
    /// The bundle's snapshot is of another head than the PR's.
    #[error("the decision bundle's snapshot is not of the PR's head")]
    Head,
    /// The bundle's inputs name another base ref than the PR's.
    #[error("the decision bundle's base ref is not the PR's")]
    BaseRef,
    /// The bundle was re-decided from another evidence pointer than the claim's.
    #[error("the decision bundle names another evidence pointer than the claim")]
    Evidence,
    /// The bundle was decided under another rubric version than the one given.
    #[error("the decision bundle was decided under another rubric version")]
    RubricVersion,
    /// The recorded decision is not the one its inputs give.
    #[error("the recorded decision is not the one its inputs give")]
    Decision(DecisionDiff),
    /// A decision could not be encoded: a defect in the domain's types.
    #[error("could not encode a decision")]
    Encode(#[source] serde_json::Error),
}

/// Why [`publish_redecided`] could not publish.
#[derive(Debug, Error)]
pub enum RedecidedError<F, L> {
    /// The bundle is not the target's re-decision; nothing was read or written.
    #[error("the decision bundle is refused")]
    Bundle(#[source] RedecidedRefused),
    /// The check could not be published.
    #[error(transparent)]
    Publish(PublishError<F, L>),
}

/// What [`publish_redecided`] returns over `ports`' types.
pub type RedecidedOutcome<F, L, S> = Result<
    Published<<L as PublishLedger>::Error, Unconfirmed<<S as DecisionBundleSource>::Error>>,
    RedecidedError<<F as VerdictForge>::Error, <L as PublishLedger>::Error>,
>;

/// Publishes the re-decision `persisted.bundle` records, over `persisted.claim`.
///
/// Before any port call the bundle must hash to `persisted.digest`, read strictly, record the
/// target's run (attempt 1), head and base, name the claim's evidence pointer, and recompute
/// under `persisted.rubric` to its recorded decision. The check is then written only while it
/// carries the claim's key (a `pending:` stamp or any other key yields, `Stale::Foreign`), and
/// is left alone when it is completed under the new key ([`Published::Unchanged`]). Under the
/// lock, after the check is re-read unchanged, the decision pointer and the register are
/// written in one compare-and-set that yields (`Stale::Moved`) when the ledger no longer holds
/// the claim; only then the check and the summary. The summary shows no LLM cost: the
/// re-decision spent none.
///
/// # Errors
///
/// [`RedecidedError::Bundle`] for a bundle that is not the target's re-decision, and
/// [`RedecidedError::Publish`] as [`publish`](super::publish) fails, or
/// [`PublishError::ClaimHead`] for a claim of another head. An unconfirmed upload is a
/// [`PersistFailure::Upload`] in the written outcome, not an error.
pub fn publish_redecided<F: VerdictForge, L: PublishLedger, S: DecisionBundleSource>(
    persisted: &RedecidedPersisted<'_>,
    ports: &RedecidedPorts<'_, F, L, S>,
) -> RedecidedOutcome<F, L, S> {
    let bytes = persisted.bundle;
    if bytes.len() > MAX_DECISION_BUNDLE_BYTES {
        return Err(RedecidedError::Bundle(RedecidedRefused::TooLarge));
    }
    if Sha256Digest::of(bytes) != *persisted.digest {
        return Err(RedecidedError::Bundle(RedecidedRefused::Digest));
    }
    let bundle = reread(persisted).map_err(RedecidedError::Bundle)?;
    let decision = bundle
        .recompute(persisted.rubric)
        .map_err(|error| RedecidedError::Bundle(RedecidedRefused::Encode(error)))?
        .map_err(|diff| RedecidedError::Bundle(RedecidedRefused::Decision(diff)))?;
    let inputs = bundle.inputs();
    let basis = Basis {
        rubric: persisted.rubric,
        inputs,
        decision: &decision,
        tier: inputs.pr.tier,
        classes: &inputs.pr.classes,
        cost: Aeu::ZERO,
        ignored_approvals: bundle.ignored_approvals(),
    };
    let mut persist = || confirm(persisted, ports.bundles);
    write::write_at(
        &persisted.target.site(),
        Claimant::Register(persisted.claim),
        Writer {
            forge: ports.forge,
            ledger: ports.ledger,
        },
        Pending::Decided {
            basis,
            persist: &mut persist,
        },
    )
    .map_err(RedecidedError::Publish)
}

/// Reads `persisted.bundle` back as the target's re-decision from the claim's evidence.
fn reread(persisted: &RedecidedPersisted<'_>) -> Result<DecisionBundle, RedecidedRefused> {
    let target = &persisted.target;
    if !target.is_first_attempt() {
        return Err(RedecidedRefused::NotFirstAttempt);
    }
    let bundle = read_decision_bundle(persisted.bundle).map_err(RedecidedRefused::Unreadable)?;
    if bundle.run_uid() != target.run_uid {
        return Err(RedecidedRefused::Run);
    }
    if bundle.head() != target.head_sha {
        return Err(RedecidedRefused::Head);
    }
    if bundle.inputs().pr.base_ref != target.base_ref {
        return Err(RedecidedRefused::BaseRef);
    }
    if bundle.evidence() != &persisted.claim.evidence {
        return Err(RedecidedRefused::Evidence);
    }
    if bundle.rubric_version() != persisted.rubric.version() {
        return Err(RedecidedRefused::RubricVersion);
    }
    Ok(bundle)
}

/// Confirms the upload: the run's decision artifact for the PR holds a file hashing to the
/// bundle's digest.
fn confirm<L, S: DecisionBundleSource>(
    persisted: &RedecidedPersisted<'_>,
    bundles: &S,
) -> Result<Sha256Digest, PersistFailure<L, Unconfirmed<S::Error>>> {
    let unconfirmed = |why| Err(PersistFailure::Upload(why));
    if persisted.upload != PersistOutcome::Success {
        return unconfirmed(Unconfirmed::NotUploaded(persisted.upload));
    }
    let target = &persisted.target;
    let stored = bundles
        .fetch_decision(target.run_uid, target.number, MAX_DECISION_BUNDLE_BYTES)
        .map_err(|error| PersistFailure::Upload(Unconfirmed::Fetch(error)))?;
    match stored {
        StoredBundle::Found(bytes) if bytes.len() > MAX_DECISION_BUNDLE_BYTES => {
            unconfirmed(Unconfirmed::TooLarge)
        }
        StoredBundle::Found(bytes) if Sha256Digest::of(&bytes) == *persisted.digest => {
            // The decision pointer owns its copy of the digest.
            Ok(persisted.digest.clone())
        }
        StoredBundle::Found(_) => unconfirmed(Unconfirmed::Digest),
        StoredBundle::Missing => unconfirmed(Unconfirmed::Missing),
        StoredBundle::Expired => unconfirmed(Unconfirmed::Expired),
        StoredBundle::TooLarge => unconfirmed(Unconfirmed::TooLarge),
    }
}
