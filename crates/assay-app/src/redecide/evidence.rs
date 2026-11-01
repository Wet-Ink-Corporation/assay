//! The evidence read (J4, F19, F20, D11): the bundle the ledger's evidence pointer names, read
//! only after GitHub says its run is one assay believes, and authenticated as replay
//! authenticates it.
//!
//! Every verdict about the bundle is a value of [`EvidenceRead`], and nothing read from GitHub
//! or the bundle is echoed in it (wi-b5affa): a forged one refuses the head as
//! `gate-error:bundle-digest`, an absent one as `bundle-expired` (02 §5: absence is not
//! forgery), and one decided under another rubric version asks for a re-run. Only a port that
//! cannot be read is an [`EvidenceError`].

use std::num::NonZeroU64;

use assay_domain::{Decision, DecisionInputs, Rubric, RunUid};
use thiserror::Error;

use crate::config::{CommitId, Sha256Digest};
use crate::eligibility::{PriorWaivers, RecordedState, WaiverBindings};
use crate::provenance::{ExpectedRun, ProvenanceRefusal, RunProvenance, Workflow, accept_run};
use crate::publish::{BUNDLE_FILE, EvidencePointer, MAX_BUNDLE_BYTES};
use crate::replay::{self, Bundle, BundleShape, BundleSource, Mismatch, StoredBundle};

/// The evidence to read: the pointer of a repository's PR state, and what to read it against.
#[derive(Debug, Clone, Copy)]
pub struct EvidenceRequest<'a> {
    /// The repository the pointer was recorded for.
    pub repo_id: NonZeroU64,
    /// The evidence pointer `{run_id, run_attempt, sha256}` from the PR's ledger state (R3-08).
    pub pointer: &'a EvidencePointer,
    /// The repository's default branch, the only branch a dispatched pipeline run may run on.
    pub default_branch: &'a str,
    /// This build's rubric.
    pub rubric: &'a Rubric,
}

/// What the evidence read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceRead {
    /// The bundle is the pipeline's and authenticates. Boxed, as it is far larger than the
    /// other verdicts.
    Authentic(Box<EvidenceBundle>),
    /// The pointer or the bundle is not what assay wrote (F19): `gate-error:bundle-digest`.
    Forged(Forgery),
    /// The bundle is gone (F20): `bundle-expired`.
    Absent(Absence),
    /// The bundle authenticates but was decided under another rubric version than this
    /// build's: a re-run decides the head (D11).
    OtherRubric,
}

/// How a pointer or its bundle is not what assay wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forgery {
    /// The pointer's run is not one whose bundle assay believes.
    Provenance(ProvenanceRefusal),
    /// The bundle is larger than [`MAX_BUNDLE_BYTES`]: assay never writes one.
    TooLarge,
    /// The bundle hashes to its pointer but is not `assay.run-inputs/1`.
    Shape(BundleShape),
    /// The bundle disagrees with its pointer (its digest or run), with itself (a document or
    /// the evidence key), or with the decision its inputs recompute to.
    Mismatch(Mismatch),
}

/// Why the bundle is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Absence {
    /// No such artifact or file.
    Missing,
    /// The artifact is past its retention.
    Expired,
}

/// Why the evidence could not be read. Never a verdict about the bundle.
#[derive(Debug, Error)]
pub enum EvidenceError<P, S> {
    /// The run's provenance could not be read.
    #[error("could not read the run's provenance")]
    Provenance(#[source] P),
    /// The artifact store could not be read.
    #[error("could not read the artifact store")]
    Fetch(#[source] S),
    /// A decision could not be encoded: a defect in `decide`'s types.
    #[error("could not encode a decision")]
    Encode(#[source] serde_json::Error),
}

/// An authenticated evidence bundle, as a re-decision reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceBundle {
    pointer: EvidencePointer,
    recorded: RecordedState,
    inputs: DecisionInputs,
    decision: Decision,
    bindings: WaiverBindings,
    prior_waivers: PriorWaivers,
}

impl EvidenceBundle {
    /// The pointer the bundle was read through.
    #[must_use]
    pub const fn pointer(&self) -> &EvidencePointer {
        &self.pointer
    }

    /// The head the bundle's run decided: its GitHub snapshot's.
    #[must_use]
    pub const fn head(&self) -> &CommitId {
        self.recorded.head()
    }

    /// What the bundle's GitHub snapshot recorded of who may act on the head (D3 A).
    #[must_use]
    pub const fn recorded(&self) -> &RecordedState {
        &self.recorded
    }

    /// The base branch the bundle's run decided against.
    #[must_use]
    pub fn base_ref(&self) -> &str {
        &self.inputs.pr.base_ref
    }

    /// The decision inputs the bundle records.
    #[must_use]
    pub const fn inputs(&self) -> &DecisionInputs {
        &self.inputs
    }

    /// The decision the bundle records, which its inputs recompute to under this build's
    /// rubric.
    #[must_use]
    pub const fn decision(&self) -> &Decision {
        &self.decision
    }

    /// What a waiver of each gate fingerprint of the bundle's head is bound to there.
    #[must_use]
    pub const fn bindings(&self) -> &WaiverBindings {
        &self.bindings
    }

    /// What the bundle's decision waived, each fingerprint with its binding, as
    /// [`prior_waivers`](crate::prior_waivers) reads it.
    #[must_use]
    pub const fn prior_waivers(&self) -> &PriorWaivers {
        &self.prior_waivers
    }
}

/// Reads the evidence bundle `request.pointer` names, after its run's provenance.
///
/// A pointer to a re-run is [`Forgery::Provenance`] with no port call. Otherwise the run is read
/// from `provenance` first, and must pass [`accept_run`] as a [`Workflow::Pipeline`] run of
/// `request.repo_id` with `request.default_branch`; no artifact is fetched for any other. The
/// bundle is then fetched under [`MAX_BUNDLE_BYTES`] and must hash to the pointer's sha256,
/// parse strictly as `assay.run-inputs/1`, record the pointer's run, keep documents that hash
/// to their digests and an evidence key equal to its inputs', and, when decided under
/// `request.rubric`'s version, recompute to its recorded decision byte for byte.
///
/// # Examples
///
/// ```
/// use assay_app::{
///     BundleSource, EvidenceError, EvidenceRead, EvidenceRequest, RunProvenance, read_evidence,
/// };
///
/// /// Whether the head may be re-decided from its evidence.
/// fn usable<P: RunProvenance, S: BundleSource>(
///     request: &EvidenceRequest<'_>,
///     provenance: &P,
///     bundles: &S,
/// ) -> Result<bool, EvidenceError<P::Error, S::Error>> {
///     Ok(matches!(
///         read_evidence(request, provenance, bundles)?,
///         EvidenceRead::Authentic(_)
///     ))
/// }
/// ```
///
/// # Errors
///
/// [`EvidenceError::Provenance`] or [`EvidenceError::Fetch`] when a port cannot be read, and
/// [`EvidenceError::Encode`] when a decision cannot be encoded. Every verdict about the bundle
/// is an [`EvidenceRead`].
pub fn read_evidence<P: RunProvenance, S: BundleSource>(
    request: &EvidenceRequest<'_>,
    provenance: &P,
    bundles: &S,
) -> Result<EvidenceRead, EvidenceError<P::Error, S::Error>> {
    let pointer = request.pointer;
    let run = RunUid {
        repo_id: request.repo_id,
        run_id: pointer.run_id,
        run_attempt: pointer.run_attempt,
    };
    if run.run_attempt.get() != 1 {
        return Ok(forged(Forgery::Provenance(ProvenanceRefusal::Attempt)));
    }
    let expected = ExpectedRun {
        workflow: Workflow::Pipeline,
        run,
        default_branch: request.default_branch,
    };
    let refusal = match provenance
        .workflow_run(run.run_id)
        .map_err(EvidenceError::Provenance)?
    {
        None => Err(ProvenanceRefusal::Missing),
        Some(facts) => accept_run(&facts, &expected),
    };
    if let Err(refusal) = refusal {
        return Ok(forged(Forgery::Provenance(refusal)));
    }
    let bytes = match bundles
        .fetch(run, BUNDLE_FILE, MAX_BUNDLE_BYTES)
        .map_err(EvidenceError::Fetch)?
    {
        StoredBundle::Found(bytes) if bytes.len() > MAX_BUNDLE_BYTES => {
            return Ok(forged(Forgery::TooLarge));
        }
        StoredBundle::Found(bytes) => bytes,
        StoredBundle::TooLarge => return Ok(forged(Forgery::TooLarge)),
        StoredBundle::Missing => return Ok(EvidenceRead::Absent(Absence::Missing)),
        StoredBundle::Expired => return Ok(EvidenceRead::Absent(Absence::Expired)),
    };
    let digest = Sha256Digest::of(&bytes);
    if digest != pointer.sha256 {
        return Ok(forged(Forgery::Mismatch(Mismatch::Digest {
            // The verdict owns both digests.
            pointer: pointer.sha256.clone(),
            bundle: digest,
        })));
    }
    let bundle = match replay::read(&bytes) {
        Ok(bundle) => bundle,
        Err(shape) => return Ok(forged(Forgery::Shape(shape))),
    };
    drop(bytes);
    authenticated(bundle, run, pointer, request.rubric)
}

/// The verdict on a parsed bundle of `run` that hashes to `pointer`.
fn authenticated<P, S>(
    bundle: Bundle,
    run: RunUid,
    pointer: &EvidencePointer,
    rubric: &Rubric,
) -> Result<EvidenceRead, EvidenceError<P, S>> {
    if let Some(mismatch) = replay::inconsistency(&bundle, run) {
        return Ok(forged(Forgery::Mismatch(mismatch)));
    }
    let Some(recorded) = RecordedState::read(&bundle.github) else {
        return Ok(forged(Forgery::Shape(BundleShape::Shape)));
    };
    if bundle.rubric_version != rubric.version() {
        return Ok(EvidenceRead::OtherRubric);
    }
    let decision = match replay::recompute(&bundle, rubric).map_err(EvidenceError::Encode)? {
        Ok(decision) => decision,
        Err(diff) => return Ok(forged(Forgery::Mismatch(Mismatch::Decision(diff)))),
    };
    let bindings = WaiverBindings::of(&bundle.inputs.gates, bundle.gate_documents());
    // Both are kept: the head's bindings for the waivers a re-decision judges, and the subset
    // the recorded decision waived, which `PriorWaivers` takes by value.
    let prior_waivers = PriorWaivers::new(
        decision.waived.iter().map(|waived| waived.fp),
        bindings.clone(),
    );
    Ok(EvidenceRead::Authentic(Box::new(EvidenceBundle {
        // The bundle outlives the request it was read for.
        pointer: pointer.clone(),
        recorded,
        inputs: bundle.inputs,
        decision,
        bindings,
        prior_waivers,
    })))
}

/// The forged verdict.
const fn forged(forgery: Forgery) -> EvidenceRead {
    EvidenceRead::Forged(forgery)
}
