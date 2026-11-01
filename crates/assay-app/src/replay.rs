//! Decision replay (R-09, ADR-15, `docs/architecture/04-pipeline.md` §8): `assay replay
//! --verify` reads the `RunInputs` bundle `decide` persisted (`assay.run-inputs/1`, 03 §6a)
//! through its evidence pointer, recomputes the decision and compares it with the recorded one,
//! byte for byte.
//!
//! The scope is the decision. Model outputs are replayed only as recorded (the bundle's review
//! documents, already reduced to the decision inputs' findings); nothing re-runs a model, so the
//! model stage is reported as recorded and non-reproducible ([`ModelReplay`]).
//!
//! Only [`Replayed::Match`] is a pass. A bundle that cannot be read (missing, expired, over the
//! cap, not `assay.run-inputs/1`) is a [`ReplayError`]; one that was read and disagrees with its
//! pointer, with itself or with the recomputed decision is a [`Mismatch`] (02 §5: absence is
//! not forgery). Nothing read from the bundle is echoed in either (wi-b5affa).

mod diff;

use std::fmt;
use std::num::NonZeroU64;

use assay_domain::{Decision, DecisionInputs, Rubric, RunUid, decide};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;
use serde_json::error::Category;
use thiserror::Error;

pub use self::diff::DecisionDiff;
use crate::config::{CommitId, Sha256Digest};
use crate::contract::parse_json_strict;
use crate::publish::{BUNDLE_FILE, EvidencePointer, MAX_BUNDLE_BYTES};
use crate::rereview::record::CarryIn;

/// The workflow runs' artifacts, read to replay a decision. Implemented by the forge adapter.
pub trait BundleSource {
    /// Why the artifact store could not be read.
    type Error: std::error::Error;

    /// The bytes of `file` in the artifact the `decide` job of `run` uploaded, read under
    /// `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the artifact store cannot be read; an absent, expired or oversized
    /// file is a [`StoredBundle`] instead.
    fn fetch(&self, run: RunUid, file: &str, limit: usize) -> Result<StoredBundle, Self::Error>;
}

/// The decision bundles decide-only runs persisted (D5), read to replay a re-decision.
/// Implemented by the forge adapter.
pub trait DecisionBundleSource {
    /// Why the artifact store could not be read.
    type Error: std::error::Error;

    /// The bytes of [`DECISION_FILE`](crate::DECISION_FILE) in the artifact decide-only run
    /// `run` uploaded for PR `pr` (`assay-decision-<pr>-<nonce>`), read under `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the artifact store cannot be read or the run's artifacts for `pr`
    /// are ambiguous; an absent, expired or oversized file is a [`StoredBundle`] instead.
    fn fetch_decision(
        &self,
        run: RunUid,
        pr: NonZeroU64,
        limit: usize,
    ) -> Result<StoredBundle, Self::Error>;
}

/// What the artifact store holds for a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredBundle {
    /// The file's bytes, not yet authenticated.
    Found(Vec<u8>),
    /// No such artifact or file: deleted by a writer, or never uploaded (F20).
    Missing,
    /// The artifact is past its 90-day retention.
    Expired,
    /// The file is larger than the limit and was not read.
    TooLarge,
}

/// The decision to replay: the evidence pointer of a repository's PR state, and the rubric.
#[derive(Debug, Clone, Copy)]
pub struct ReplayRequest<'a> {
    /// The repository the pointer was recorded for.
    pub repo_id: NonZeroU64,
    /// The evidence pointer `{run_id, run_attempt, sha256}` from the PR's ledger state (R3-08).
    pub pointer: &'a EvidencePointer,
    /// The rubric whose version the bundle records.
    pub rubric: &'a Rubric,
}

/// What a replay found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replayed {
    /// The recomputed decision is the recorded one, byte for byte: the only pass. Boxed, as the
    /// decision is far larger than a mismatch.
    Match(Box<Matched>),
    /// The bundle was read and something in it disagrees.
    Mismatch(Mismatch),
}

/// A decision that replays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    /// The decision, recomputed.
    pub decision: Decision,
    /// How the model stage was replayed.
    pub model: ModelReplay,
}

/// How the model stage was replayed: from recorded outputs only, never by re-running a model,
/// so a model-level result is not reproducible (R-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelReplay {
    /// The bundle's recorded review documents stand for the model stage; no model ran.
    RecordedOnly {
        /// How many review documents the bundle records.
        review_documents: usize,
    },
}

/// Where a read bundle disagrees with its pointer, with itself or with the recomputed decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// The bundle's bytes do not hash to the pointer's sha256: the bundle or the pointer was
    /// changed, or the bundle was cut short (F19).
    Digest {
        /// The sha256 the pointer records.
        pointer: Sha256Digest,
        /// The sha256 of the bytes read.
        bundle: Sha256Digest,
    },
    /// The bundle records another run than the pointer names.
    Run(RunField),
    /// A document's text no longer hashes to the sha256 recorded beside it.
    Document {
        /// The document's index in the bundle.
        index: usize,
    },
    /// The bundle's evidence key is not its decision inputs'.
    EvidenceKey,
    /// The recomputed decision differs from the recorded one.
    Decision(DecisionDiff),
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Digest { pointer, bundle } => write!(
                f,
                "the bundle hashes to {bundle}, not to the pointer's {pointer}"
            ),
            Self::Run(field) => write!(
                f,
                "the bundle records another {} than the pointer",
                field.name()
            ),
            Self::Document { index } => write!(
                f,
                "document {index} of the bundle no longer hashes to its recorded sha256"
            ),
            Self::EvidenceKey => f.write_str("the bundle's evidence key is not its inputs'"),
            Self::Decision(diff) => write!(
                f,
                "the recomputed decision differs from the recorded one at byte {} of its \
                 canonical encoding, field {:?}",
                diff.offset, diff.field
            ),
        }
    }
}

/// A field of the run a bundle records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunField {
    /// `repo_id`.
    RepoId,
    /// `run_id`.
    RunId,
    /// `run_attempt`.
    RunAttempt,
}

impl RunField {
    /// The field's name in `run_uid`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RepoId => "repo_id",
            Self::RunId => "run_id",
            Self::RunAttempt => "run_attempt",
        }
    }
}

/// Why authenticated bytes are not an `assay.run-inputs/1` bundle. Positions are 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BundleShape {
    /// The JSON ends early: the bundle was cut short before it was hashed.
    Truncated {
        /// The line the text ended on.
        line: usize,
        /// The column the text ended at.
        column: usize,
    },
    /// The text is not JSON.
    Syntax {
        /// The line of the error.
        line: usize,
        /// The column of the error.
        column: usize,
    },
    /// The JSON repeats a key, or is not `assay.run-inputs/1` (another version, a missing or
    /// unknown field, a value of the wrong type).
    Shape,
}

impl fmt::Display for BundleShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { line, column } => {
                write!(f, "it ends early, at line {line}, column {column}")
            }
            Self::Syntax { line, column } => {
                write!(f, "it is not JSON at line {line}, column {column}")
            }
            Self::Shape => f.write_str("it repeats a key or does not have the contract's fields"),
        }
    }
}

/// Why a decision could not be replayed. Never a pass.
#[derive(Debug, Error)]
pub enum ReplayError<E> {
    /// The pointer names a re-run: bundles are persisted by attempt 1 only (R3-05). Nothing
    /// was fetched.
    #[error("refused attempt {attempt}: a bundle pointer names attempt 1 only")]
    NotFirstAttempt {
        /// The attempt the pointer names.
        attempt: NonZeroU64,
    },
    /// The artifact store could not be read.
    #[error("could not read the artifact store")]
    Fetch(#[source] E),
    /// The bundle is gone (F20); a re-run is required.
    #[error("the bundle is missing; a re-run is required")]
    Missing,
    /// The bundle is past its 90-day retention; a re-run is required.
    #[error("the bundle has expired; a re-run is required")]
    Expired,
    /// The bundle is larger than [`MAX_BUNDLE_BYTES`] and was not read on.
    #[error("the bundle is larger than {MAX_BUNDLE_BYTES} bytes")]
    TooLarge,
    /// The bundle hashes to its pointer but is not `assay.run-inputs/1`.
    #[error("the bundle is not assay.run-inputs/1: {0}")]
    Unreadable(BundleShape),
    /// The bundle was decided under another rubric version than the one given.
    #[error("the bundle was decided under another rubric version")]
    RubricVersion,
    /// A decision could not be encoded: a defect in `decide`'s types.
    #[error("could not encode a decision")]
    Encode(#[source] serde_json::Error),
}

/// `assay.run-inputs/1` as replay (and `assay status`) reads it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bundle {
    #[serde(rename = "schema")]
    _schema: BundleSchema,
    run_uid: RunUid,
    evidence_key: String,
    documents: Vec<BundleDocument>,
    pub(crate) inputs: DecisionInputs,
    pub(crate) rubric_version: String,
    pub(crate) decision: Value,
    /// Checked to be an array, not kept: replay does not read events.
    #[serde(rename = "events")]
    _events: Vec<IgnoredAny>,
    /// The findings the run carries into the next (#224); replay does not read them.
    pub(crate) carry: CarryIn,
    /// The GitHub state snapshot (#58). Required; replay recomputes the decision from `inputs`,
    /// which already hold what the rubric read of it, and only the next run's admit reads its
    /// `head_sha` (#60 ST-11, D6 A), so it is kept unparsed.
    pub(crate) github: Value,
}

/// The only bundle version this build reads.
#[derive(Deserialize)]
enum BundleSchema {
    #[serde(rename = "assay.run-inputs/1")]
    V1,
}

/// One authenticated document as the bundle keeps it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleDocument {
    job: DocumentJob,
    file: String,
    sha256: String,
    text: String,
}

impl Bundle {
    /// The head the bundle's GitHub snapshot was taken of; `None` when its `head_sha` is not
    /// a commit id.
    pub(crate) fn snapshot_head(&self) -> Option<CommitId> {
        self.github.get("head_sha")?.as_str()?.parse().ok()
    }

    /// Each gate document's file and bytes, as its job published them, in bundle order: what a
    /// waiver was bound to on the bundle's head (#59).
    pub(crate) fn gate_documents(&self) -> impl Iterator<Item = (&str, &[u8])> + '_ {
        self.documents
            .iter()
            .filter(|document| {
                matches!(
                    document.job,
                    DocumentJob::GatesStatic | DocumentJob::GatesLint | DocumentJob::GatesTest
                )
            })
            .map(|document| (document.file.as_str(), document.text.as_bytes()))
    }
}

/// The job that published a document.
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum DocumentJob {
    Plan,
    GatesStatic,
    GatesLint,
    GatesTest,
    Review,
}

/// Replays the decision `request`'s pointer names, reading its bundle from `source`.
///
/// The bundle is fetched under [`MAX_BUNDLE_BYTES`] and must hash to the pointer's sha256,
/// record the pointer's run, keep documents that still hash to their recorded digests and
/// an evidence key equal to its inputs', and recompute under `request.rubric` to the recorded
/// decision. The decisions are compared in their canonical encoding (compact JSON, keys
/// sorted), byte for byte.
///
/// # Examples
///
/// ```
/// use assay_app::{BundleSource, ReplayError, ReplayRequest, Replayed, replay_decision};
///
/// /// Whether the decision replays: only a match passes.
/// fn verified<S: BundleSource>(
///     request: &ReplayRequest<'_>,
///     source: &S,
/// ) -> Result<bool, ReplayError<S::Error>> {
///     Ok(matches!(replay_decision(request, source)?, Replayed::Match(_)))
/// }
/// ```
///
/// # Errors
///
/// [`ReplayError::NotFirstAttempt`] for a pointer to a re-run, before any fetch; otherwise the
/// store failed ([`ReplayError::Fetch`]), the bundle is gone ([`ReplayError::Missing`],
/// [`ReplayError::Expired`]), over the cap ([`ReplayError::TooLarge`]) or not the contract
/// ([`ReplayError::Unreadable`]), the rubric is another version
/// ([`ReplayError::RubricVersion`]), or a decision could not be encoded
/// ([`ReplayError::Encode`]). A disagreement is a [`Replayed::Mismatch`], not an error.
pub fn replay_decision<S: BundleSource>(
    request: &ReplayRequest<'_>,
    source: &S,
) -> Result<Replayed, ReplayError<S::Error>> {
    let pointer = request.pointer;
    let attempt = pointer.run_attempt;
    if attempt.get() != 1 {
        return Err(ReplayError::NotFirstAttempt { attempt });
    }
    let run = RunUid {
        repo_id: request.repo_id,
        run_id: pointer.run_id,
        run_attempt: attempt,
    };
    let bytes = fetch(source, run)?;
    let digest = Sha256Digest::of(&bytes);
    if digest != pointer.sha256 {
        return Ok(Replayed::Mismatch(Mismatch::Digest {
            // The report owns both digests.
            pointer: pointer.sha256.clone(),
            bundle: digest,
        }));
    }
    let bundle = read(&bytes).map_err(ReplayError::Unreadable)?;
    drop(bytes);
    if let Some(mismatch) = inconsistency(&bundle, run) {
        return Ok(Replayed::Mismatch(mismatch));
    }
    if bundle.rubric_version != request.rubric.version() {
        return Err(ReplayError::RubricVersion);
    }
    match recompute(&bundle, request.rubric).map_err(ReplayError::Encode)? {
        Err(diff) => Ok(Replayed::Mismatch(Mismatch::Decision(diff))),
        Ok(decision) => Ok(Replayed::Match(Box::new(Matched {
            decision,
            model: ModelReplay::RecordedOnly {
                review_documents: bundle
                    .documents
                    .iter()
                    .filter(|document| document.job == DocumentJob::Review)
                    .count(),
            },
        }))),
    }
}

/// The bundle's bytes, read under the cap; the adapter is held to it too.
fn fetch<S: BundleSource>(source: &S, run: RunUid) -> Result<Vec<u8>, ReplayError<S::Error>> {
    match source
        .fetch(run, BUNDLE_FILE, MAX_BUNDLE_BYTES)
        .map_err(ReplayError::Fetch)?
    {
        StoredBundle::Found(bytes) if bytes.len() > MAX_BUNDLE_BYTES => Err(ReplayError::TooLarge),
        StoredBundle::Found(bytes) => Ok(bytes),
        StoredBundle::Missing => Err(ReplayError::Missing),
        StoredBundle::Expired => Err(ReplayError::Expired),
        StoredBundle::TooLarge => Err(ReplayError::TooLarge),
    }
}

/// The decision `bundle`'s inputs give under `rubric`, when it is the one the bundle records in
/// canonical encoding; otherwise where the two first differ.
///
/// # Errors
///
/// A decision could not be encoded: a defect in `decide`'s types.
pub(crate) fn recompute(
    bundle: &Bundle,
    rubric: &Rubric,
) -> Result<Result<Decision, DecisionDiff>, serde_json::Error> {
    recompute_recorded(&bundle.inputs, &bundle.decision, rubric)
}

/// The decision `inputs` give under `rubric`, when it is `recorded` in canonical encoding;
/// otherwise where the two first differ. One rule for both bundle contracts.
///
/// # Errors
///
/// A decision could not be encoded: a defect in `decide`'s types.
pub(crate) fn recompute_recorded(
    inputs: &DecisionInputs,
    recorded: &Value,
    rubric: &Rubric,
) -> Result<Result<Decision, DecisionDiff>, serde_json::Error> {
    let decision = decide(rubric, inputs);
    let replayed = serde_json::to_value(&decision)?;
    Ok(match diff::first_difference(recorded, &replayed)? {
        Some(diff) => Err(diff),
        None => Ok(decision),
    })
}

/// Parses authenticated bytes as `assay.run-inputs/1`, refusing a repeated key.
pub(crate) fn read(bytes: &[u8]) -> Result<Bundle, BundleShape> {
    read_value(bytes).and_then(of_value)
}

/// `value` as `assay.run-inputs/1`.
pub(crate) fn of_value(value: Value) -> Result<Bundle, BundleShape> {
    serde_json::from_value(value).map_err(|_not_run_inputs| BundleShape::Shape)
}

/// Parses authenticated bytes as JSON, refusing a repeated key.
pub(crate) fn read_value(bytes: &[u8]) -> Result<Value, BundleShape> {
    parse_json_strict(bytes).map_err(|error| {
        let (line, column) = (error.line(), error.column());
        match error.classify() {
            Category::Eof => BundleShape::Truncated { line, column },
            Category::Syntax | Category::Io => BundleShape::Syntax { line, column },
            Category::Data => BundleShape::Shape,
        }
    })
}

/// The first field of the run a bundle `recorded` that is not the pointer's `run`'s, if any.
/// One rule for both bundle contracts.
pub(crate) fn other_run(recorded: RunUid, run: RunUid) -> Option<RunField> {
    if recorded.repo_id != run.repo_id {
        Some(RunField::RepoId)
    } else if recorded.run_id != run.run_id {
        Some(RunField::RunId)
    } else if recorded.run_attempt != run.run_attempt {
        Some(RunField::RunAttempt)
    } else {
        None
    }
}

/// Where the bundle disagrees with the pointer's `run` or with itself, if anywhere.
pub(crate) fn inconsistency(bundle: &Bundle, run: RunUid) -> Option<Mismatch> {
    if let Some(field) = other_run(bundle.run_uid, run) {
        return Some(Mismatch::Run(field));
    }
    let tampered = bundle.documents.iter().position(|document| {
        Sha256Digest::of(document.text.as_bytes()).as_str() != document.sha256
    });
    if let Some(index) = tampered {
        return Some(Mismatch::Document { index });
    }
    (bundle.evidence_key != bundle.inputs.evidence_key).then_some(Mismatch::EvidenceKey)
}
