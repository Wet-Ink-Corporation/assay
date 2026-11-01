//! Seeding a run's carry set from the previous completed run's bundle
//! (`docs/architecture/04-pipeline.md` §7, #224): the bundle is read through its evidence
//! pointer, authenticated as replay and `assay status` authenticate it, and its carried
//! findings go through the carried intake. The same bundle names the waivers its decision
//! applied, which a later head may carry (#59), and the head it was decided on (#60 ST-11, D6
//! A). A reset or a first review reads nothing.

use std::num::NonZeroU64;

use assay_domain::{Decision, RunUid};
use serde::Deserialize;
use thiserror::Error;

use super::carried::{CarriedFinding, CarryRefused, read_carried};
use super::target::Carry;
use crate::config::{CommitId, Sha256Digest};
use crate::eligibility::{PriorWaivers, WaiverBindings};
use crate::publish::{BUNDLE_FILE, EvidencePointer, MAX_BUNDLE_BYTES};
use crate::replay::{self, Bundle, BundleShape, BundleSource, Mismatch, StoredBundle};

/// Where the previous completed run's bundle is, and the repository the findings are for.
#[derive(Debug, Clone, Copy)]
pub struct PriorCarry<'a> {
    /// The repository's numeric id.
    pub repo_id: NonZeroU64,
    /// The ledger's evidence pointer of the PR's last completed run, the run whose head
    /// [`TargetRequest::previous`](super::TargetRequest::previous) names.
    pub pointer: &'a EvidencePointer,
    /// The repository, `owner/name`, for the injection screen.
    pub repository: &'a str,
}

/// Why the previous run's carry set could not be read. The caller decides what the run does;
/// nothing was carried.
#[derive(Debug, Error)]
pub enum PriorCarryError<E> {
    /// The pointer names a re-run: bundles are persisted by attempt 1 only (R3-05). Nothing
    /// was fetched.
    #[error("the evidence pointer names a re-run")]
    NotFirstAttempt,
    /// The artifact store could not be read.
    #[error("could not read the artifact store")]
    Fetch(#[source] E),
    /// The bundle is gone (F20).
    #[error("the bundle is missing")]
    Missing,
    /// The bundle is past its retention.
    #[error("the bundle has expired")]
    Expired,
    /// The bundle is larger than [`MAX_BUNDLE_BYTES`] and was not read on.
    #[error("the bundle is over its size cap")]
    TooLarge,
    /// The bundle does not hash to the pointer (F19).
    #[error("the bundle does not hash to the evidence pointer")]
    Digest,
    /// The bundle hashes to its pointer but is not `assay.run-inputs/1`.
    #[error("the bundle is not assay.run-inputs/1: {0}")]
    Shape(BundleShape),
    /// The bundle names another run than the pointer, or disagrees with itself: where.
    #[error("the bundle disagrees with its pointer or with itself: {0}")]
    Inconsistent(Mismatch),
    /// The carried intake refused the set.
    #[error("the bundle's carry set is refused")]
    Refused(#[source] CarryRefused),
}

/// The findings this run carries, read from the bundle `request.pointer` names.
///
/// None when `carry` is not [`Carry::Kept`] (a first review, a rebase or force-push, an
/// unreachable previous head), with nothing fetched; otherwise the bundle's carry set, in its
/// order.
///
/// The bundle is fetched under [`MAX_BUNDLE_BYTES`], must hash to the pointer's sha256, parse
/// strictly as `assay.run-inputs/1` (a repeated key or an unknown field is refused), record the
/// pointer's run and keep documents that hash to their digests; its findings then pass
/// [`read_carried`] for `request.repository`. Dismissals are not in the carry set and are
/// never touched.
///
/// # Errors
///
/// [`PriorCarryError::NotFirstAttempt`] for a pointer to a re-run, before any fetch; otherwise
/// the store failed ([`PriorCarryError::Fetch`]), the bundle is gone
/// ([`PriorCarryError::Missing`], [`PriorCarryError::Expired`]), over the cap
/// ([`PriorCarryError::TooLarge`]), not the pointer's ([`PriorCarryError::Digest`],
/// [`PriorCarryError::Inconsistent`]), not the contract ([`PriorCarryError::Shape`]), or its
/// carry set is refused ([`PriorCarryError::Refused`]).
pub fn prior_carry<S: BundleSource>(
    carry: Carry,
    request: &PriorCarry<'_>,
    source: &S,
) -> Result<Vec<CarriedFinding>, PriorCarryError<S::Error>> {
    match carry {
        Carry::Kept => {}
        Carry::FirstReview | Carry::Reset(_) => return Ok(Vec::new()),
    }
    let bundle = read_prior(request, source)?;
    read_carried(bundle.carry.into_parts(), request.repository).map_err(PriorCarryError::Refused)
}

/// The head of the previous completed run, from the bundle `request.pointer` names.
///
/// It is the bundle's GitHub state snapshot's `head_sha` (#60 ST-11, D6 A): the ledger's
/// evidence pointer names no head, and this is the run whose findings [`prior_carry`] carries.
///
/// The bundle is fetched and authenticated exactly as [`prior_carry`] does.
///
/// # Errors
///
/// As [`prior_carry`], except [`PriorCarryError::Refused`], which this never returns; a
/// snapshot whose `head_sha` is not a commit id is [`PriorCarryError::Shape`].
pub fn prior_head<S: BundleSource>(
    request: &PriorCarry<'_>,
    source: &S,
) -> Result<CommitId, PriorCarryError<S::Error>> {
    let bundle = read_prior(request, source)?;
    bundle
        .snapshot_head()
        .ok_or(PriorCarryError::Shape(BundleShape::Shape))
}

/// What the previous completed run's decision waived, read from the bundle `request.pointer`
/// names (R3-13, #59).
///
/// Each waived fingerprint comes with its binding there, for
/// [`GitHubState::waivers_carried`](crate::GitHubState::waivers_carried).
///
/// Nothing when `carry` is not [`Carry::Kept`], with nothing fetched: a rebase or force-push
/// carries no waiver, as it carries no finding. Otherwise the bundle is fetched and
/// authenticated exactly as [`prior_carry`] does, and its decision must read as
/// `assay.decision/1`. A waived fingerprint is bound to the binding digests (canonical JSON
/// without `source.run_key`, wi-bc5341) of the bundle's gate documents of every gate carrying
/// it, as [`AuthenticatedRun::waiver_bindings`](crate::AuthenticatedRun::waiver_bindings) binds
/// this run's.
///
/// # Errors
///
/// As [`prior_carry`], except [`PriorCarryError::Refused`], which this never returns; a
/// decision that does not read is [`PriorCarryError::Shape`].
pub fn prior_waivers<S: BundleSource>(
    carry: Carry,
    request: &PriorCarry<'_>,
    source: &S,
) -> Result<PriorWaivers, PriorCarryError<S::Error>> {
    match carry {
        Carry::Kept => {}
        Carry::FirstReview | Carry::Reset(_) => return Ok(PriorWaivers::default()),
    }
    let bundle = read_prior(request, source)?;
    let decision = Decision::deserialize(&bundle.decision)
        .map_err(|_not_a_decision| PriorCarryError::Shape(BundleShape::Shape))?;
    let bindings = WaiverBindings::of(&bundle.inputs.gates, bundle.gate_documents());
    Ok(PriorWaivers::new(
        decision.waived.iter().map(|waived| waived.fp),
        bindings,
    ))
}

/// Fetches and authenticates the bundle `request.pointer` names: attempt 1 only, under
/// [`MAX_BUNDLE_BYTES`], hashing to the pointer, strictly `assay.run-inputs/1`, recording the
/// pointer's run and keeping documents that hash to their digests.
fn read_prior<S: BundleSource>(
    request: &PriorCarry<'_>,
    source: &S,
) -> Result<Bundle, PriorCarryError<S::Error>> {
    let pointer = request.pointer;
    if pointer.run_attempt.get() != 1 {
        return Err(PriorCarryError::NotFirstAttempt);
    }
    let run = RunUid {
        repo_id: request.repo_id,
        run_id: pointer.run_id,
        run_attempt: pointer.run_attempt,
    };
    let bytes = match source
        .fetch(run, BUNDLE_FILE, MAX_BUNDLE_BYTES)
        .map_err(PriorCarryError::Fetch)?
    {
        StoredBundle::Found(bytes) if bytes.len() > MAX_BUNDLE_BYTES => {
            return Err(PriorCarryError::TooLarge);
        }
        StoredBundle::Found(bytes) => bytes,
        StoredBundle::Missing => return Err(PriorCarryError::Missing),
        StoredBundle::Expired => return Err(PriorCarryError::Expired),
        StoredBundle::TooLarge => return Err(PriorCarryError::TooLarge),
    };
    if Sha256Digest::of(&bytes) != pointer.sha256 {
        return Err(PriorCarryError::Digest);
    }
    let bundle = replay::read(&bytes).map_err(PriorCarryError::Shape)?;
    drop(bytes);
    if let Some(mismatch) = replay::inconsistency(&bundle, run) {
        return Err(PriorCarryError::Inconsistent(mismatch));
    }
    Ok(bundle)
}
