//! `decide`'s publication (S8) and persistence (S9), `docs/architecture/02-architecture.md` §5
//! and `04-pipeline.md` §6: the one verdict check, written under the per-head ledger lock and
//! only while the stamp the writer read still stands (R3-09); the summary comment; the
//! `RunInputs` bundle, its evidence pointer and evidence record (R3-08, R3-17).
//!
//! A superseded or yielding run writes nothing (R3-06); its ledger cost record and slot release
//! are the `release` job's, never `decide`'s. An App check that already carries the decision key
//! with a final conclusion is left alone, with no other call. Persisting fails closed for
//! trust-path and exec-surface PRs (`failure persist-error`) and is a visible warning otherwise.

mod bundle;
mod ports;
mod render;
mod write;

use assay_domain::{Aeu, Conclusion, ContentHash, DecisionInputs, Event, Reason, Rubric, RunUid};
use thiserror::Error;

pub use self::bundle::{BUNDLE_FILE, BundleError, MAX_BUNDLE_BYTES};
pub use self::ports::{
    BundleStore, CheckConclusion, CheckName, CheckRun, CheckRunId, CheckState, CheckStatus,
    CheckUpdate, CommentId, EvidenceEntry, EvidencePointer, EvidenceRecord, LlmEvidence,
    PublishLedger, SummaryComment, VerdictForge,
};
pub use self::render::SUMMARY_MARKER;
use self::render::{Projection, Summary};
use self::write::Pending;
use crate::config::Sha256Digest;
use crate::decide::{AuthenticatedRun, Refusal};
use crate::plan::PullRequestEvent;

/// What `decide` publishes for one run.
#[derive(Debug)]
pub struct Publication<'a> {
    /// The run and the PR it publishes for.
    pub target: Target<'a>,
    /// What it concluded.
    pub verdict: Verdict<'a>,
}

/// The run that publishes and the PR it publishes for.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// This run (attempt 1: `authenticate` refuses any other).
    pub run_uid: RunUid,
    /// The PR as the run was planned for it, whose head and base ref `authenticate` re-checked.
    pub pull_request: &'a PullRequestEvent,
    /// The repository's default branch, which names the check (R3-03).
    pub default_branch: &'a str,
}

/// What `decide` concluded about a run.
#[derive(Debug, Clone, Copy)]
pub enum Verdict<'a> {
    /// The run was superseded, or yields to a newer run: write nothing at all (R3-06).
    Silent,
    /// `authenticate` refused the run: publish `failure` with its reason. Nothing it read is
    /// authenticated, so no bundle or evidence is persisted.
    Refused(&'a Refusal),
    /// The rubric decides over authenticated inputs.
    Decided(Decided<'a>),
}

/// An authenticated run and what the rubric is given about it.
#[derive(Debug, Clone, Copy)]
pub struct Decided<'a> {
    /// The run `authenticate` established.
    pub run: &'a AuthenticatedRun,
    /// Rubric v1.
    pub rubric: &'a Rubric,
    /// The decision inputs built over `run`: `evidence_key` is its run key and
    /// `decision_key` is `sha256:<64 hex>` (#58 computes it).
    pub inputs: &'a DecisionInputs,
    /// Every job's events, merged into the bundle in `(job, idx)` order.
    pub events: &'a [Event],
    /// What the run's LLM stage cost.
    pub cost: Aeu,
}

/// The ports `publish` writes through, borrowed for one call.
#[derive(Debug)]
pub struct PublishPorts<'p, F, L, B> {
    /// The verdict check and the summary comment.
    pub forge: &'p F,
    /// The per-head lock and the PR's ledger state.
    pub ledger: &'p L,
    /// The run's artifacts.
    pub bundles: &'p B,
}

/// What `publish` did.
#[derive(Debug)]
pub enum Published<L, B> {
    /// Nothing: the run was superseded or yields to a newer one.
    Nothing,
    /// Nothing: the App's check already carries this decision key with a final conclusion.
    Unchanged,
    /// Nothing: the check is no longer this run's to write (R3-09).
    Yielded {
        /// Why.
        stale: Stale,
        /// What became of the lock.
        unlock: Unlock<L>,
    },
    /// The check, and the summary comment where it differed, were written.
    Written(Written<L, B>),
}

/// Why a writer yielded (R3-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stale {
    /// The check's stamp is neither this run's `pending:` stamp nor its decision key: the lock
    /// was not taken.
    Foreign,
    /// The check changed between the writer's first read and its read under the lock.
    Moved,
}

/// What was written.
#[derive(Debug)]
pub struct Written<L, B> {
    /// The check's new state.
    pub state: CheckState,
    /// Why the bundle or its evidence was not persisted, if it was not. On a trust-path or
    /// exec-surface PR the check then fails with `persist-error`; otherwise its summary warns.
    pub persist: Option<PersistFailure<L, B>>,
    /// What became of the lock.
    pub unlock: Unlock<L>,
}

/// Why the bundle or its evidence was not persisted (S9).
#[derive(Debug)]
pub enum PersistFailure<L, B> {
    /// The bundle could not be built.
    Bundle(BundleError),
    /// The bundle could not be uploaded; no pointer was written.
    Upload(B),
    /// The bundle was uploaded but its pointer and record could not be written.
    Ledger(L),
}

impl<L, B> PersistFailure<L, B> {
    /// The word the summary and the `persist-error` reason use for it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Bundle(BundleError::NotUtf8 { .. }) => "bundle-not-utf8",
            Self::Bundle(BundleError::TooLarge) => "bundle-too-large",
            Self::Bundle(BundleError::Encode(_)) => "bundle-encode",
            Self::Upload(_) => "upload",
            Self::Ledger(_) => "ledger",
        }
    }
}

/// What became of the per-head lock.
#[derive(Debug)]
pub enum Unlock<L> {
    /// It was never taken.
    NotTaken,
    /// It was released.
    Released,
    /// Releasing it failed; it expires with its TTL.
    Failed(L),
}

/// The forge call that failed while the lock was held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForgeStep {
    /// Re-reading the check under the lock.
    ReadCheck,
    /// Writing the check.
    UpdateCheck,
    /// Finding the summary comment.
    FindSummary,
    /// Posting or editing the summary comment.
    WriteSummary,
}

impl ForgeStep {
    const fn describe(self) -> &'static str {
        match self {
            Self::ReadCheck => "re-read the verdict check",
            Self::UpdateCheck => "write the verdict check",
            Self::FindSummary => "find the summary comment",
            Self::WriteSummary => "write the summary comment",
        }
    }
}

/// Why `publish` could not publish. The job fails; `reconcile` later closes a check left
/// `in_progress` as `failure publish-error` (06 §4).
#[derive(Debug, Error)]
pub enum PublishError<F, L> {
    /// The decision key is not `sha256:<64 hex>`, so it could be mistaken for a stamp.
    #[error("the decision key is not a sha256 digest")]
    DecisionKey,
    /// The decision inputs were built over another run's evidence.
    #[error("the decision inputs' evidence key is not the run's")]
    EvidenceKey,
    /// The App's check for the head does not exist: `open` creates it, never `decide`.
    #[error("there is no verdict check for the head")]
    CheckMissing,
    /// The check could not be read before taking the lock.
    #[error("could not read the verdict check")]
    Observe(#[source] F),
    /// The lock could not be taken.
    #[error("could not take the per-head publish lock")]
    Lock(#[source] L),
    /// Another writer held the lock past the adapter's wait.
    #[error("the per-head publish lock is held by another writer")]
    LockBusy,
    /// A forge call failed while the lock was held.
    #[error("could not {} under the lock", step.describe())]
    Locked {
        /// The call.
        step: ForgeStep,
        /// The forge's error.
        #[source]
        source: F,
        /// What became of the lock.
        unlock: Unlock<L>,
    },
}

/// The [`PublishError`] of [`publish`] over `ports`' error types.
pub type PublishFailure<F, L> =
    PublishError<<F as VerdictForge>::Error, <L as PublishLedger>::Error>;

/// What [`publish`] returns over `ports`' error types.
pub type PublishOutcome<F, L, B> = Result<
    Published<<L as PublishLedger>::Error, <B as BundleStore>::Error>,
    PublishFailure<F, L>,
>;

/// Publishes `publication`'s verdict through `ports`.
///
/// # Examples
///
/// ```
/// use assay_app::{
///     BundleStore, PublishLedger, PublishOutcome, PublishPorts, Publication, Published,
///     VerdictForge, publish,
/// };
///
/// # fn demo<F: VerdictForge, L: PublishLedger, B: BundleStore>(
/// #     publication: &Publication<'_>,
/// #     ports: &PublishPorts<'_, F, L, B>,
/// # ) -> PublishOutcome<F, L, B> {
/// let published = publish(publication, ports)?;
/// if let Published::Written(written) = &published {
///     // Report `written.persist` and `written.unlock`; a trust-path PR's check already
///     // fails on a persist error.
///     drop(&written.persist);
/// }
/// # Ok(published)
/// # }
/// ```
///
/// # Errors
///
/// [`PublishError`] when the inputs do not fit together ([`PublishError::DecisionKey`],
/// [`PublishError::EvidenceKey`]), there is no check to write, the lock cannot be taken, or a
/// forge call fails. A stale writer yields ([`Published::Yielded`]); that is not an error.
pub fn publish<F: VerdictForge, L: PublishLedger, B: BundleStore>(
    publication: &Publication<'_>,
    ports: &PublishPorts<'_, F, L, B>,
) -> PublishOutcome<F, L, B> {
    let target = &publication.target;
    match publication.verdict {
        Verdict::Silent => Ok(Published::Nothing),
        Verdict::Refused(refusal) => write::write(target, ports, Pending::Refused(refused(target, refusal))),
        Verdict::Decided(decided) => {
            let inputs = decided.inputs;
            if inputs.decision_key.parse::<Sha256Digest>().is_err() {
                return Err(PublishError::DecisionKey);
            }
            if inputs.evidence_key != decided.run.run_key().to_string() {
                return Err(PublishError::EvidenceKey);
            }
            let decision = assay_domain::decide(decided.rubric, inputs);
            write::write(
                target,
                ports,
                Pending::Decided {
                    decided: &decided,
                    decision: &decision,
                },
            )
        }
    }
}

/// The check update for a refusal: `failure` with its reason, keyed by the run and the reason.
fn refused(target: &Target<'_>, refusal: &Refusal) -> CheckUpdate {
    let reason = refusal.reason();
    let key = refusal_key(target.run_uid, &reason);
    let reasons = [reason];
    CheckUpdate {
        state: CheckState::Completed(CheckConclusion::Failure),
        title: render::title(Conclusion::Failure, &reasons),
        summary: render::summary(&Summary {
            conclusion: Conclusion::Failure,
            reasons: &reasons,
            facts: None,
            warning: None,
        }),
        text: render::text(&Projection {
            decision_key: &key,
            inputs_digest: None,
            evidence: None,
        }),
        external_id: key,
    }
}

/// The key a refusal is published under, `sha256:<hex>` over the run and the reason: the same
/// refusal of the same run has the same key.
fn refusal_key(run_uid: RunUid, reason: &Reason) -> String {
    let repo_id = run_uid.repo_id.to_string();
    let run_id = run_uid.run_id.to_string();
    let run_attempt = run_uid.run_attempt.to_string();
    ContentHash::of_files([
        ("refusal", reason.code.code().as_bytes()),
        ("detail", reason.detail.as_bytes()),
        ("repo_id", repo_id.as_bytes()),
        ("run_id", run_id.as_bytes()),
        ("run_attempt", run_attempt.as_bytes()),
    ])
    .to_string()
}
