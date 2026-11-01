//! Replay follows the chain (J10, R-09, `docs/architecture/04-pipeline.md` §8): a decide-only
//! decision bundle (`assay.run-decision/1`) is replayed through the evidence bundle it names.
//!
//! The decision pointer's run must be attempt 1 of `assay-decide.yml` (provenance read before
//! any fetch); its bundle must hash to the pointer and record its run; the evidence pointer it
//! names is read by [`read_evidence`], the one authentication of an evidence bundle; the
//! decision's inputs must keep every field of the evidence's that a GitHub snapshot does not
//! decide; and the recorded decision must recompute byte for byte. The fields a snapshot
//! decides (owner approval, authorship, requeues, waivers, override, decision key) are taken as
//! the bundle recorded them: GitHub as it was then cannot be read again.
//!
//! Only [`ChainReplayed::Match`] is a pass. As in [`replay_decision`](crate::replay_decision),
//! absence and unreadability are [`ChainError`]s and a disagreement is a [`ChainMismatch`];
//! nothing read is echoed in either (wi-b5affa).

use std::fmt;
use std::num::NonZeroU64;

use assay_domain::{Decision, DecisionInputs, PullRequest, Rubric, RunUid};
use thiserror::Error;

use super::evidence::{Absence, EvidenceBundle, EvidenceError, EvidenceRead, EvidenceRequest};
use super::read::{DecisionBundle, DecisionBundleError, read_decision_bundle};
use super::{Forgery, MAX_DECISION_BUNDLE_BYTES, read_evidence};
use crate::config::Sha256Digest;
use crate::ledger::DecisionPointer;
use crate::provenance::{ExpectedRun, ProvenanceRefusal, RunProvenance, Workflow, accept_run};
use crate::replay::{
    BundleShape, BundleSource, DecisionBundleSource, DecisionDiff, RunField, StoredBundle,
    other_run,
};

/// The chain to replay: a PR's decision pointer, and what to read it against.
#[derive(Debug, Clone, Copy)]
pub struct ChainRequest<'a> {
    /// The repository the pointer was recorded for.
    pub repo_id: NonZeroU64,
    /// The PR whose ledger state holds the pointer; its decision bundle's artifact is named
    /// for it.
    pub pr: NonZeroU64,
    /// The decision pointer `{run_id, sha256}` from the PR's ledger state.
    pub pointer: &'a DecisionPointer,
    /// The repository's default branch, the only branch decide-only's comment and schedule
    /// runs, and a dispatched pipeline run, may run on.
    pub default_branch: &'a str,
    /// The rubric whose version both bundles record.
    pub rubric: &'a Rubric,
}

/// The chain's ports: GitHub's runs, the pipeline's artifacts and decide-only's artifacts.
#[derive(Debug, Clone, Copy)]
pub struct ChainPorts<'a, P, E, D> {
    /// Reads a run's provenance.
    pub provenance: &'a P,
    /// Reads the evidence bundle.
    pub evidence: &'a E,
    /// Reads the decision bundle.
    pub decisions: &'a D,
}

/// What a chain replay found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainReplayed {
    /// The decision bundle recomputes to its recorded decision through its evidence: the only
    /// pass. Boxed, as it is far larger than a mismatch.
    Match(Box<ChainMatched>),
    /// The chain is broken somewhere.
    Mismatch(ChainMismatch),
}

/// A decision bundle that replays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainMatched {
    /// The re-decision, recomputed.
    pub decision: Decision,
    /// The evidence bundle it was re-decided from, authenticated (its own decision replays).
    pub evidence: EvidenceBundle,
}

/// Where the chain from a decision pointer to the recomputed decision breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainMismatch {
    /// The decision pointer's run is not one whose bundle assay believes: not attempt 1 of
    /// `assay-decide.yml` of this repository (F19). Nothing was fetched.
    Provenance(ProvenanceRefusal),
    /// The decision bundle's bytes do not hash to the pointer's sha256.
    Digest {
        /// The sha256 the pointer records.
        pointer: Sha256Digest,
        /// The sha256 of the bytes read.
        bundle: Sha256Digest,
    },
    /// The decision bundle records another run than the pointer names.
    Run(RunField),
    /// The evidence pointer the decision bundle names, or its bundle, is forged.
    Evidence(Forgery),
    /// The evidence bundle was decided under another rubric version than the decision bundle,
    /// which decide-only never writes.
    EvidenceRubric,
    /// The decision bundle's snapshot is of another head than the evidence's.
    Head,
    /// The decision's inputs differ from the evidence's on a field no snapshot decides.
    Inputs(InputsField),
    /// The recomputed decision differs from the recorded one.
    Decision(DecisionDiff),
}

impl fmt::Display for ChainMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provenance(refusal) => write!(
                f,
                "the decision pointer's run is not decide-only's: {refusal}"
            ),
            Self::Digest { pointer, bundle } => write!(
                f,
                "the decision bundle hashes to {bundle}, not to the pointer's {pointer}"
            ),
            Self::Run(field) => write!(
                f,
                "the decision bundle records another {} than the pointer",
                field.name()
            ),
            Self::Evidence(_) => f.write_str("the evidence the decision bundle names is forged"),
            Self::EvidenceRubric => f.write_str(
                "the evidence was decided under another rubric version than the decision",
            ),
            Self::Head => f.write_str("the decision bundle is of another head than its evidence"),
            Self::Inputs(field) => write!(
                f,
                "the decision's inputs differ from the evidence's on {}",
                field.name()
            ),
            Self::Decision(diff) => write!(
                f,
                "the recomputed decision differs from the recorded one at byte {} of its \
                 canonical encoding, field {:?}",
                diff.offset, diff.field
            ),
        }
    }
}

/// A decision input a re-decision keeps from its evidence (no GitHub snapshot decides it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputsField {
    /// `evidence_key`.
    EvidenceKey,
    /// `policy`.
    Policy,
    /// `pr.base_ref`.
    BaseRef,
    /// `pr.open_prs_on_head`.
    OpenPrsOnHead,
    /// `pr.tier`.
    Tier,
    /// `pr.classes`.
    Classes,
    /// `pr.readiness` (D15).
    Readiness,
    /// `gates`.
    Gates,
    /// `weigh_in`.
    WeighIn,
    /// `llm`.
    Llm,
    /// `findings`.
    Findings,
    /// `dispositions`.
    Dispositions,
    /// `completed_roles`.
    CompletedRoles,
    /// `coordinator`.
    Coordinator,
    /// `verifier`.
    Verifier,
    /// `degraded`.
    Degraded,
}

impl InputsField {
    /// The field's path in `assay.run-decision/1`'s `inputs`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::EvidenceKey => "evidence_key",
            Self::Policy => "policy",
            Self::BaseRef => "pr.base_ref",
            Self::OpenPrsOnHead => "pr.open_prs_on_head",
            Self::Tier => "pr.tier",
            Self::Classes => "pr.classes",
            Self::Readiness => "pr.readiness",
            Self::Gates => "gates",
            Self::WeighIn => "weigh_in",
            Self::Llm => "llm",
            Self::Findings => "findings",
            Self::Dispositions => "dispositions",
            Self::CompletedRoles => "completed_roles",
            Self::Coordinator => "coordinator",
            Self::Verifier => "verifier",
            Self::Degraded => "degraded",
        }
    }
}

/// Why a chain could not be replayed. Never a pass.
#[derive(Debug, Error)]
pub enum ChainError<P, E, D> {
    /// A run's provenance could not be read.
    #[error("could not read a run's provenance")]
    Provenance(#[source] P),
    /// The pipeline's artifact store could not be read.
    #[error("could not read the evidence bundle's artifact store")]
    EvidenceFetch(#[source] E),
    /// decide-only's artifact store could not be read.
    #[error("could not read the decision bundle's artifact store")]
    DecisionFetch(#[source] D),
    /// The decision bundle is gone.
    #[error("the decision bundle is missing")]
    Missing,
    /// The decision bundle is past its 90-day retention.
    #[error("the decision bundle has expired")]
    Expired,
    /// The decision bundle is larger than [`MAX_DECISION_BUNDLE_BYTES`] and was not read on.
    #[error("the decision bundle is larger than {MAX_DECISION_BUNDLE_BYTES} bytes")]
    TooLarge,
    /// The decision bundle hashes to its pointer but is not `assay.run-decision/1` as assay
    /// writes it.
    #[error("the decision bundle is not assay.run-decision/1: {0}")]
    Unreadable(BundleShape),
    /// The evidence bundle the decision names is gone (F20).
    #[error("the evidence bundle is gone")]
    EvidenceAbsent(Absence),
    /// The decision bundle was decided under another rubric version than the one given.
    #[error("the decision bundle was decided under another rubric version")]
    RubricVersion,
    /// A decision could not be encoded: a defect in `decide`'s types.
    #[error("could not encode a decision")]
    Encode(#[source] serde_json::Error),
}

/// Why [`replay_chain`] could not replay over ports `P`, `E` and `D`.
type Failure<P, E, D> = ChainError<
    <P as RunProvenance>::Error,
    <E as BundleSource>::Error,
    <D as DecisionBundleSource>::Error,
>;

/// Replays the decision bundle `request.pointer` names through the evidence bundle it names.
///
/// The decision run's provenance is read first and must pass [`accept_run`] as a
/// [`Workflow::DecideOnly`] run of `request.repo_id`; nothing is fetched for any other run.
/// The bundle is then fetched for `request.pr` under [`MAX_DECISION_BUNDLE_BYTES`], must hash
/// to the pointer's sha256, read strictly as `assay.run-decision/1`, record the pointer's run
/// and `request.rubric`'s version. Its evidence pointer is read by [`read_evidence`], which
/// must find the evidence authentic, decided under the same rubric version and of the same
/// head; the decision's inputs must equal the evidence's on every field a re-decision keeps
/// ([`InputsField`]); and the recorded decision must recompute byte for byte.
///
/// # Examples
///
/// ```
/// use assay_app::{
///     BundleSource, ChainError, ChainPorts, ChainReplayed, ChainRequest, DecisionBundleSource,
///     RunProvenance, replay_chain,
/// };
///
/// /// Whether the decision replays through its evidence: only a match passes.
/// fn verified<P: RunProvenance, E: BundleSource, D: DecisionBundleSource>(
///     request: &ChainRequest<'_>,
///     ports: &ChainPorts<'_, P, E, D>,
/// ) -> Result<bool, ChainError<P::Error, E::Error, D::Error>> {
///     Ok(matches!(replay_chain(request, ports)?, ChainReplayed::Match(_)))
/// }
/// ```
///
/// # Errors
///
/// A port cannot be read ([`ChainError::Provenance`], [`ChainError::DecisionFetch`],
/// [`ChainError::EvidenceFetch`]); the decision bundle is gone ([`ChainError::Missing`],
/// [`ChainError::Expired`]), over the cap ([`ChainError::TooLarge`]) or not the contract
/// ([`ChainError::Unreadable`]); the rubric is another version ([`ChainError::RubricVersion`]);
/// the evidence is gone ([`ChainError::EvidenceAbsent`]); or a decision could not be encoded
/// ([`ChainError::Encode`]). A disagreement is a [`ChainReplayed::Mismatch`], not an error.
pub fn replay_chain<P: RunProvenance, E: BundleSource, D: DecisionBundleSource>(
    request: &ChainRequest<'_>,
    ports: &ChainPorts<'_, P, E, D>,
) -> Result<ChainReplayed, Failure<P, E, D>> {
    let decision = match decision_bundle(request, ports)? {
        Ok(decision) => decision,
        Err(mismatch) => return Ok(ChainReplayed::Mismatch(mismatch)),
    };
    let evidence = match evidence_of(&decision, request, ports)? {
        Ok(evidence) => evidence,
        Err(mismatch) => return Ok(ChainReplayed::Mismatch(mismatch)),
    };
    if decision.head() != evidence.head() {
        return Ok(ChainReplayed::Mismatch(ChainMismatch::Head));
    }
    if let Some(field) = disagreement(decision.inputs(), evidence.inputs()) {
        return Ok(ChainReplayed::Mismatch(ChainMismatch::Inputs(field)));
    }
    Ok(
        match decision
            .recompute(request.rubric)
            .map_err(ChainError::Encode)?
        {
            Err(diff) => ChainReplayed::Mismatch(ChainMismatch::Decision(diff)),
            Ok(recomputed) => ChainReplayed::Match(Box::new(ChainMatched {
                decision: recomputed,
                evidence: *evidence,
            })),
        },
    )
}

/// The decision bundle `request.pointer` names, read after its run's provenance; or where it
/// disagrees with its pointer.
fn decision_bundle<P: RunProvenance, E: BundleSource, D: DecisionBundleSource>(
    request: &ChainRequest<'_>,
    ports: &ChainPorts<'_, P, E, D>,
) -> Result<Result<DecisionBundle, ChainMismatch>, Failure<P, E, D>> {
    let pointer = request.pointer;
    let run = RunUid {
        repo_id: request.repo_id,
        run_id: pointer.run_id,
        run_attempt: NonZeroU64::MIN,
    };
    let expected = ExpectedRun {
        workflow: Workflow::DecideOnly,
        run,
        default_branch: request.default_branch,
    };
    let believed = match ports
        .provenance
        .workflow_run(run.run_id)
        .map_err(ChainError::Provenance)?
    {
        None => Err(ProvenanceRefusal::Missing),
        Some(facts) => accept_run(&facts, &expected),
    };
    if let Err(refusal) = believed {
        return Ok(Err(ChainMismatch::Provenance(refusal)));
    }
    let bytes = fetched(ports.decisions, run, request.pr)?;
    let digest = Sha256Digest::of(&bytes);
    if digest != pointer.sha256 {
        return Ok(Err(ChainMismatch::Digest {
            // The report owns both digests.
            pointer: pointer.sha256.clone(),
            bundle: digest,
        }));
    }
    let decision = read_decision_bundle(&bytes).map_err(|error| match error {
        DecisionBundleError::TooLarge => ChainError::TooLarge,
        DecisionBundleError::Unreadable(shape) => ChainError::Unreadable(shape),
    })?;
    drop(bytes);
    if let Some(field) = other_run(decision.run_uid(), run) {
        return Ok(Err(ChainMismatch::Run(field)));
    }
    if decision.rubric_version() != request.rubric.version() {
        return Err(ChainError::RubricVersion);
    }
    Ok(Ok(decision))
}

/// The decision bundle's bytes of `run` for `pr`, read under the cap; the adapter is held to
/// it too.
fn fetched<P, E, D: DecisionBundleSource>(
    decisions: &D,
    run: RunUid,
    pr: NonZeroU64,
) -> Result<Vec<u8>, ChainError<P, E, D::Error>> {
    match decisions
        .fetch_decision(run, pr, MAX_DECISION_BUNDLE_BYTES)
        .map_err(ChainError::DecisionFetch)?
    {
        StoredBundle::Found(bytes) if bytes.len() > MAX_DECISION_BUNDLE_BYTES => {
            Err(ChainError::TooLarge)
        }
        StoredBundle::Found(bytes) => Ok(bytes),
        StoredBundle::Missing => Err(ChainError::Missing),
        StoredBundle::Expired => Err(ChainError::Expired),
        StoredBundle::TooLarge => Err(ChainError::TooLarge),
    }
}

/// The evidence bundle `decision` names, authenticated by [`read_evidence`]; or how it is
/// forged.
fn evidence_of<P: RunProvenance, E: BundleSource, D: DecisionBundleSource>(
    decision: &DecisionBundle,
    request: &ChainRequest<'_>,
    ports: &ChainPorts<'_, P, E, D>,
) -> Result<Result<Box<EvidenceBundle>, ChainMismatch>, Failure<P, E, D>> {
    let read = read_evidence(
        &EvidenceRequest {
            repo_id: request.repo_id,
            pointer: decision.evidence(),
            default_branch: request.default_branch,
            rubric: request.rubric,
        },
        ports.provenance,
        ports.evidence,
    )
    .map_err(|error| match error {
        EvidenceError::Provenance(source) => ChainError::Provenance(source),
        EvidenceError::Fetch(source) => ChainError::EvidenceFetch(source),
        EvidenceError::Encode(source) => ChainError::Encode(source),
    })?;
    match read {
        EvidenceRead::Authentic(evidence) => Ok(Ok(evidence)),
        EvidenceRead::Forged(forgery) => Ok(Err(ChainMismatch::Evidence(forgery))),
        EvidenceRead::OtherRubric => Ok(Err(ChainMismatch::EvidenceRubric)),
        EvidenceRead::Absent(absence) => Err(ChainError::EvidenceAbsent(absence)),
    }
}

/// The first field a re-decision keeps from its evidence on which `decided` differs from
/// `evidence`, if any. Both are destructured in full, so a new input does not compile until it
/// is classified as kept or decided by a snapshot.
fn disagreement(decided: &DecisionInputs, evidence: &DecisionInputs) -> Option<InputsField> {
    let DecisionInputs {
        decision_key: _,
        evidence_key,
        policy,
        pr,
        gates,
        weigh_in,
        llm,
        findings,
        dispositions,
        completed_roles,
        coordinator,
        verifier,
        degraded,
        waivers: _,
        override_record: _,
    } = decided;
    let kept = [
        (
            evidence_key == &evidence.evidence_key,
            InputsField::EvidenceKey,
        ),
        (policy == &evidence.policy, InputsField::Policy),
    ];
    let rest = [
        (gates == &evidence.gates, InputsField::Gates),
        (weigh_in == &evidence.weigh_in, InputsField::WeighIn),
        (llm == &evidence.llm, InputsField::Llm),
        (findings == &evidence.findings, InputsField::Findings),
        (
            dispositions == &evidence.dispositions,
            InputsField::Dispositions,
        ),
        (
            completed_roles == &evidence.completed_roles,
            InputsField::CompletedRoles,
        ),
        (
            coordinator == &evidence.coordinator,
            InputsField::Coordinator,
        ),
        (verifier == &evidence.verifier, InputsField::Verifier),
        (degraded == &evidence.degraded, InputsField::Degraded),
    ];
    first_unequal(kept)
        .or_else(|| pr_disagreement(pr, &evidence.pr))
        .or_else(|| first_unequal(rest))
}

/// Like [`disagreement`], for the PR facts.
fn pr_disagreement(decided: &PullRequest, evidence: &PullRequest) -> Option<InputsField> {
    let PullRequest {
        base_ref,
        open_prs_on_head,
        tier,
        classes,
        owner_approval: _,
        authorship: _,
        readiness,
        requeues: _,
    } = decided;
    first_unequal([
        (base_ref == &evidence.base_ref, InputsField::BaseRef),
        (
            open_prs_on_head == &evidence.open_prs_on_head,
            InputsField::OpenPrsOnHead,
        ),
        (tier == &evidence.tier, InputsField::Tier),
        (classes == &evidence.classes, InputsField::Classes),
        (readiness == &evidence.readiness, InputsField::Readiness),
    ])
}

/// The field of the first comparison that is not equal, if any.
fn first_unequal(compared: impl IntoIterator<Item = (bool, InputsField)>) -> Option<InputsField> {
    compared
        .into_iter()
        .find_map(|(equal, field)| (!equal).then_some(field))
}
