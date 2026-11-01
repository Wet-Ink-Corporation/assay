//! Publishing from a persisted decision (D5 A, 02 §5, S9): `assay decide` builds the bundle with
//! [`prepare`] and writes it; the workflow's `upload-artifact` step uploads it; `assay publish`
//! hands the same bytes to [`publish_persisted`], which re-reads everything it publishes from
//! them, confirms the upload through [`BundleSource`] by sha256, and then takes the lock, records
//! the evidence pointer and writes the check exactly as [`publish`](super::publish) does.
//!
//! No API a workflow step can reach uploads an Actions artifact, so the upload happens between
//! two processes; the evidence pointer is still written only after the upload is confirmed, so
//! a pointer never names a bundle that is not there (fail closed, wi-b5affa).

use std::fmt;
use std::str::FromStr;

use assay_domain::{Aeu, Rubric, Tier};
use serde::Deserialize;
use thiserror::Error;

use super::bundle::{self, BUNDLE_FILE, BundleError, Contents, MAX_BUNDLE_BYTES};
use super::ports::{PublishLedger, VerdictForge};
use super::write::{self, Basis, Pending, Writer};
use super::{Decided, PersistFailure, PublishError, Published, Target};
use crate::config::Sha256Digest;
use crate::eligibility::StateRecordIn;
use crate::replay::{
    self, BundleShape, BundleSource, DecisionDiff, Mismatch, RunField, StoredBundle,
};

/// The bundle [`prepare`] built: the exact bytes `decide` writes as [`BUNDLE_FILE`] and their
/// sha256, which `decide` publishes as its `bundle_digest` output.
///
/// decide-only's [`prepare_decision`](crate::prepare_decision) builds its decision bundle as one
/// too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBundle {
    bytes: Vec<u8>,
    digest: Sha256Digest,
}

impl PreparedBundle {
    /// `bytes`, a bundle that read back, with their sha256.
    pub(crate) fn of(bytes: Vec<u8>) -> Self {
        let digest = Sha256Digest::of(&bytes);
        Self { bytes, digest }
    }

    /// The bundle's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Their sha256.
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    /// The bytes, for writing without a copy.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Why [`prepare`] built no bundle. Nothing is persisted or published: the job fails, and the
/// check `open` left `in_progress` is closed as `failure publish-error` by the reconcile (06 §4).
#[derive(Debug, Error)]
pub enum PrepareError {
    /// The decision key is not `sha256:<64 hex>`, so it could be mistaken for a stamp.
    #[error("the decision key is not a sha256 digest")]
    DecisionKey,
    /// The decision inputs were built over another run's evidence.
    #[error("the decision inputs' evidence key is not the run's")]
    EvidenceKey,
    /// The GitHub state snapshot was taken of another head than the run's.
    #[error("the GitHub state snapshot is not of the run's head")]
    SnapshotHead,
    /// The inputs' base ref is not the PR's.
    #[error("the decision inputs' base ref is not the PR's")]
    BaseRef,
    /// The inputs' classes are not the run's, so the bundle would not say which PRs fail
    /// closed on a persist failure.
    #[error("the decision inputs' classes are not the run's")]
    Classes,
    /// The inputs' tier is neither the run's nor `needs-split`, so the bundle would not say the
    /// tier the LLM stage ran at.
    #[error("the decision inputs' tier is not the run's")]
    Tier,
    /// The bundle could not be built.
    #[error("could not build the bundle")]
    Bundle(#[source] BundleError),
    /// The bundle built does not read back as what it was built from: a defect in `decide`.
    #[error("the bundle does not read back: {0}")]
    ReadBack(#[source] BundleRefused),
}

/// Builds `decided`'s `assay.run-inputs/1` bundle for `target` and its sha256, after the checks
/// [`publish`](super::publish) makes, and only when [`publish_persisted`] can publish from it.
///
/// # Errors
///
/// [`PrepareError`] when the inputs do not fit the run or the target, or the bundle cannot be
/// built or read back.
pub fn prepare(target: &Target<'_>, decided: &Decided<'_>) -> Result<PreparedBundle, PrepareError> {
    let (run, inputs) = (decided.run, decided.inputs);
    if inputs.decision_key.parse::<Sha256Digest>().is_err() {
        return Err(PrepareError::DecisionKey);
    }
    if inputs.evidence_key != run.run_key().to_string() {
        return Err(PrepareError::EvidenceKey);
    }
    if decided.github.head() != &target.pull_request.head_sha {
        return Err(PrepareError::SnapshotHead);
    }
    if inputs.pr.base_ref != target.pull_request.base_ref {
        return Err(PrepareError::BaseRef);
    }
    if inputs.pr.classes != run.tiering().classes {
        return Err(PrepareError::Classes);
    }
    if inputs.pr.tier != run.tier() && inputs.pr.tier != Tier::NeedsSplit {
        return Err(PrepareError::Tier);
    }
    let decision = assay_domain::decide(decided.rubric, inputs);
    let bytes = bundle::bundle(&Contents {
        run_uid: target.run_uid,
        run,
        inputs,
        decision: &decision,
        events: decided.events,
        carry: decided.carry,
        github: decided.github,
    })
    .map_err(PrepareError::Bundle)?;
    reread(target, decided.rubric, &bytes).map_err(PrepareError::ReadBack)?;
    Ok(PreparedBundle::of(bytes))
}

/// How the workflow's upload step ended (`steps.<id>.outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PersistOutcome {
    /// `success`: the step reports the bundle uploaded; [`publish_persisted`] still confirms it.
    Success,
    /// `failure`.
    Failure,
    /// `cancelled`.
    Cancelled,
    /// `skipped`.
    Skipped,
}

impl PersistOutcome {
    /// The outcome's word.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }
}

impl fmt::Display for PersistOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// A step outcome other than the four words GitHub Actions writes. The text is never shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("the upload outcome is not success, failure, cancelled or skipped")]
pub struct UnknownPersistOutcome;

impl FromStr for PersistOutcome {
    type Err = UnknownPersistOutcome;

    /// Exactly `success`, `failure`, `cancelled` or `skipped`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "success" => Ok(Self::Success),
            "failure" => Ok(Self::Failure),
            "cancelled" => Ok(Self::Cancelled),
            "skipped" => Ok(Self::Skipped),
            _ => Err(UnknownPersistOutcome),
        }
    }
}

/// Why an upload was not confirmed: a [`PersistFailure::Upload`].
///
/// No evidence pointer (or, for decide-only, no decision pointer) is written, a trust-path or
/// exec-surface PR fails `persist-error` and any other PR warns.
#[derive(Debug, Error)]
pub enum Unconfirmed<E> {
    /// The upload step did not succeed; the store was not read.
    #[error("the upload step ended {0}")]
    NotUploaded(PersistOutcome),
    /// The artifact store could not be read.
    #[error("could not read the artifact store")]
    Fetch(#[source] E),
    /// The store holds no such file.
    #[error("the uploaded bundle is missing")]
    Missing,
    /// The store reports the artifact past its retention.
    #[error("the uploaded bundle has expired")]
    Expired,
    /// The stored file is over its cap: [`MAX_BUNDLE_BYTES`] for a run's bundle,
    /// [`MAX_DECISION_BUNDLE_BYTES`](crate::MAX_DECISION_BUNDLE_BYTES) for a decision bundle.
    #[error("the uploaded bundle is larger than its cap")]
    TooLarge,
    /// The stored file does not hash to the bundle's digest.
    #[error("the uploaded bundle does not hash to the bundle's digest")]
    Digest,
}

/// Why a bundle handed to [`publish_persisted`] is not this run's decision. Nothing is read
/// from or written to any port. Nothing from the bundle is echoed (wi-b5affa).
#[derive(Debug, Error)]
pub enum BundleRefused {
    /// The bundle is larger than [`MAX_BUNDLE_BYTES`].
    #[error("the bundle is larger than {MAX_BUNDLE_BYTES} bytes")]
    TooLarge,
    /// The bytes do not hash to the digest given with them.
    #[error("the bundle does not hash to its digest")]
    Digest,
    /// The target is a re-run: only attempt 1 publishes (R3-05).
    #[error("only attempt 1 publishes")]
    NotFirstAttempt,
    /// The bytes are not `assay.run-inputs/1`.
    #[error("the bundle is not assay.run-inputs/1: {0}")]
    Unreadable(BundleShape),
    /// The bundle records another run than the target's.
    #[error("the bundle records another {} than the run", .0.name())]
    Run(RunField),
    /// A document no longer hashes to the sha256 recorded beside it.
    #[error("document {index} of the bundle no longer hashes to its recorded sha256")]
    Document {
        /// The document's index in the bundle.
        index: usize,
    },
    /// The bundle's evidence key is not its decision inputs'.
    #[error("the bundle's evidence key is not its inputs'")]
    EvidenceKey,
    /// The decision key is not `sha256:<64 hex>`.
    #[error("the decision key is not a sha256 digest")]
    DecisionKey,
    /// The GitHub state snapshot is of another head than the PR's.
    #[error("the bundle's snapshot is not of the PR's head")]
    Head,
    /// The inputs' base ref is not the PR's.
    #[error("the bundle's base ref is not the PR's")]
    BaseRef,
    /// The bundle was decided under another rubric version than the one given.
    #[error("the bundle was decided under another rubric version")]
    RubricVersion,
    /// The recorded decision is not the one its inputs give.
    #[error("the recorded decision is not the one its inputs give")]
    Decision(DecisionDiff),
    /// A decision could not be encoded: a defect in `decide`'s types.
    #[error("could not encode a decision")]
    Encode(#[source] serde_json::Error),
}

/// A persisted decision to publish.
#[derive(Debug, Clone, Copy)]
pub struct Persisted<'a> {
    /// The run and the PR it publishes for, from the same job inputs `decide` read.
    pub target: Target<'a>,
    /// Rubric v1, whose version the bundle must record.
    pub rubric: &'a Rubric,
    /// What the run's LLM stage cost, as `decide` was given it; shown in the summary only.
    pub cost: Aeu,
    /// The bundle's bytes as [`prepare`] built them.
    pub bundle: &'a [u8],
    /// Their sha256 as `decide` published it.
    pub digest: &'a Sha256Digest,
    /// How the upload step ended.
    pub upload: PersistOutcome,
}

/// The ports [`publish_persisted`] uses, borrowed for one call.
#[derive(Debug)]
pub struct PersistedPorts<'p, F, L, S> {
    /// The verdict check and the summary comment.
    pub forge: &'p F,
    /// The per-head lock and the PR's ledger state.
    pub ledger: &'p L,
    /// The run's uploaded artifacts.
    pub bundles: &'p S,
}

/// Why [`publish_persisted`] could not publish.
#[derive(Debug, Error)]
pub enum PersistedError<F, L> {
    /// The bundle is not this run's decision; nothing was read or written.
    #[error("the bundle is refused")]
    Bundle(#[source] BundleRefused),
    /// The check could not be published.
    #[error(transparent)]
    Publish(PublishError<F, L>),
}

/// What [`publish_persisted`] returns over `ports`' types.
pub type PersistedOutcome<F, L, S> = Result<
    Published<<L as PublishLedger>::Error, Unconfirmed<<S as BundleSource>::Error>>,
    PersistedError<<F as VerdictForge>::Error, <L as PublishLedger>::Error>,
>;

/// Publishes the decision `persisted.bundle` records.
///
/// Before any port call the bundle must hash to `persisted.digest`, record the target's run
/// (attempt 1), head and base, keep documents that hash to their recorded digests, and recompute
/// under `persisted.rubric` to its recorded decision. Then the check is read as
/// [`publish`](super::publish) reads it; when it is this run's to write, the upload is confirmed
/// (only after a `success` step, by fetching [`BUNDLE_FILE`] under [`MAX_BUNDLE_BYTES`] and
/// comparing its sha256), and under the lock the evidence (only once confirmed), the check and
/// the summary are written.
///
/// # Errors
///
/// [`PersistedError::Bundle`] for a bundle that is not this run's decision, and
/// [`PersistedError::Publish`] as [`publish`](super::publish) fails. An unconfirmed upload is a
/// [`PersistFailure::Upload`] in the written outcome, not an error.
pub fn publish_persisted<F: VerdictForge, L: PublishLedger, S: BundleSource>(
    persisted: &Persisted<'_>,
    ports: &PersistedPorts<'_, F, L, S>,
) -> PersistedOutcome<F, L, S> {
    let target = &persisted.target;
    let bytes = persisted.bundle;
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(PersistedError::Bundle(BundleRefused::TooLarge));
    }
    if Sha256Digest::of(bytes) != *persisted.digest {
        return Err(PersistedError::Bundle(BundleRefused::Digest));
    }
    let read = reread(target, persisted.rubric, bytes).map_err(PersistedError::Bundle)?;
    let basis = Basis {
        rubric: persisted.rubric,
        inputs: &read.inputs,
        decision: &read.decision,
        tier: read.inputs.pr.tier,
        classes: &read.inputs.pr.classes,
        cost: persisted.cost,
        ignored_approvals: &read.ignored_approvals,
    };
    let mut persist = || confirm(persisted, ports.bundles);
    write::write(
        target,
        Writer {
            forge: ports.forge,
            ledger: ports.ledger,
        },
        Pending::Decided {
            basis,
            persist: &mut persist,
        },
    )
    .map_err(PersistedError::Publish)
}

/// Confirms the upload: the stored file hashes to the bundle's digest.
fn confirm<L, S: BundleSource>(
    persisted: &Persisted<'_>,
    bundles: &S,
) -> Result<Sha256Digest, PersistFailure<L, Unconfirmed<S::Error>>> {
    let unconfirmed = |why| Err(PersistFailure::Upload(why));
    if persisted.upload != PersistOutcome::Success {
        return unconfirmed(Unconfirmed::NotUploaded(persisted.upload));
    }
    let stored = bundles
        .fetch(persisted.target.run_uid, BUNDLE_FILE, MAX_BUNDLE_BYTES)
        .map_err(|error| PersistFailure::Upload(Unconfirmed::Fetch(error)))?;
    match stored {
        StoredBundle::Found(bytes) if bytes.len() > MAX_BUNDLE_BYTES => {
            unconfirmed(Unconfirmed::TooLarge)
        }
        StoredBundle::Found(bytes) if Sha256Digest::of(&bytes) == *persisted.digest => {
            // The evidence pointer and record own their copies of the digest.
            Ok(persisted.digest.clone())
        }
        StoredBundle::Found(_) => unconfirmed(Unconfirmed::Digest),
        StoredBundle::Missing => unconfirmed(Unconfirmed::Missing),
        StoredBundle::Expired => unconfirmed(Unconfirmed::Expired),
        StoredBundle::TooLarge => unconfirmed(Unconfirmed::TooLarge),
    }
}

/// What a bundle says that publishing needs.
struct Reread {
    inputs: assay_domain::DecisionInputs,
    decision: assay_domain::Decision,
    ignored_approvals: Vec<crate::eligibility::IgnoredApproval>,
}

/// Reads `bytes` back as the decision of `target`'s run under `rubric`.
fn reread(target: &Target<'_>, rubric: &Rubric, bytes: &[u8]) -> Result<Reread, BundleRefused> {
    let run = target.run_uid;
    if run.run_attempt.get() != 1 {
        return Err(BundleRefused::NotFirstAttempt);
    }
    let value = replay::read_value(bytes).map_err(BundleRefused::Unreadable)?;
    let github = value
        .get("github")
        .map(StateRecordIn::deserialize)
        .and_then(Result::ok)
        .ok_or(BundleRefused::Unreadable(BundleShape::Shape))?;
    let bundle = replay::of_value(value).map_err(BundleRefused::Unreadable)?;
    match replay::inconsistency(&bundle, run) {
        Some(Mismatch::Run(field)) => return Err(BundleRefused::Run(field)),
        Some(Mismatch::Document { index }) => return Err(BundleRefused::Document { index }),
        Some(Mismatch::EvidenceKey) => return Err(BundleRefused::EvidenceKey),
        // `inconsistency` reports neither; read as the bundle disagreeing with itself.
        Some(Mismatch::Digest { .. } | Mismatch::Decision(_)) => {
            return Err(BundleRefused::Unreadable(BundleShape::Shape));
        }
        None => {}
    }
    if bundle.inputs.decision_key.parse::<Sha256Digest>().is_err() {
        return Err(BundleRefused::DecisionKey);
    }
    if github.head_sha() != target.pull_request.head_sha.as_str() {
        return Err(BundleRefused::Head);
    }
    if bundle.inputs.pr.base_ref != target.pull_request.base_ref {
        return Err(BundleRefused::BaseRef);
    }
    if bundle.rubric_version != rubric.version() {
        return Err(BundleRefused::RubricVersion);
    }
    let decision = replay::recompute(&bundle, rubric)
        .map_err(BundleRefused::Encode)?
        .map_err(BundleRefused::Decision)?;
    let ignored_approvals = github
        .ignored_approvals()
        .ok_or(BundleRefused::Unreadable(BundleShape::Shape))?;
    Ok(Reread {
        inputs: bundle.inputs,
        decision,
        ignored_approvals,
    })
}
