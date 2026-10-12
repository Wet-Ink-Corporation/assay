//! The ports `decide` publishes through: the verdict check and summary comment on GitHub, the
//! per-head lock and per-PR state in the ledger, and the run's artifact store (02 §5, 06 §1).

use std::fmt;
use std::num::NonZeroU64;

use assay_domain::Tier;

use crate::config::{CommitId, Sha256Digest};

/// The verdict check and the summary comment, written with the `assay-verdict` App's token.
/// Implemented by the forge adapter (#53).
pub trait VerdictForge {
    /// Why GitHub could not be read or written.
    type Error: std::error::Error;

    /// The check run named `name` on `head` that the `assay-verdict` App authored, if any. A
    /// same-named check from any other app is never returned.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the check runs cannot be listed.
    fn find_check(&self, head: &CommitId, name: &CheckName)
    -> Result<Option<CheckRun>, Self::Error>;

    /// Overwrites the check's status, conclusion, output and `external_id` with `update`. The
    /// update carries no annotations, so none ever accumulate on the verdict check (R3-11).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the check cannot be updated.
    fn update_check(&self, id: CheckRunId, update: &CheckUpdate) -> Result<(), Self::Error>;

    /// The comment on PR `number` that the App authored and whose body contains `marker`, if
    /// any. A comment by anyone else is never returned, whatever it contains.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comments cannot be listed.
    fn find_summary(
        &self,
        number: NonZeroU64,
        marker: &str,
    ) -> Result<Option<SummaryComment>, Self::Error>;

    /// Posts `body` as a new comment on PR `number`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comment cannot be posted.
    fn create_summary(&self, number: NonZeroU64, body: &str) -> Result<(), Self::Error>;

    /// Replaces the body of comment `id` with `body`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comment cannot be edited.
    fn update_summary(&self, id: CommentId, body: &str) -> Result<(), Self::Error>;
}

/// The ledger repository, written with the App's ledger installation token. Implemented by the
/// ledger adapter (#70).
pub trait PublishLedger {
    /// Why the ledger could not be read or written.
    type Error: std::error::Error;
    /// Proof of holding the per-head lock: what releasing it compares and sets.
    type Lease;

    /// Takes the per-head publish lock `refs/heads/lock-<repo_id>-<head>` by creating it (2-minute
    /// TTL). `None` when another writer holds it and the adapter's bounded wait ran out.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the ledger cannot be reached.
    fn acquire(&self, repo_id: NonZeroU64, head: &CommitId)
    -> Result<Option<Self::Lease>, Self::Error>;

    /// Releases the lock `lease` holds, by compare-and-set: a lock another writer took after this
    /// one expired is never released.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the lock could not be released; it then expires with its TTL.
    fn release(&self, lease: Self::Lease) -> Result<(), Self::Error>;

    /// Writes `entry`'s evidence pointer and evidence record into the PR's state
    /// `state/<repo_id>/<pr>.json`, by compare-and-set on the file.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the state file could not be written.
    fn record_evidence(&self, entry: &EvidenceEntry) -> Result<(), Self::Error>;
}

/// The run's artifact store, written with the run's token. Implemented by the forge adapter.
pub trait BundleStore {
    /// Why the artifact could not be uploaded.
    type Error: std::error::Error;

    /// Uploads `bytes` as `file` of a new artifact named `artifact`, kept 90 days.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the upload failed.
    fn upload(&self, artifact: &str, file: &str, bytes: &[u8]) -> Result<(), Self::Error>;
}

/// The verdict check's name: `assay / verdict` on the default branch, `assay / verdict @<base>`
/// on any other base, so a verdict never passes for a base it was not decided for (R3-03).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckName(String);

impl CheckName {
    /// The name of the verdict check for a PR into `base_ref` in a repository whose default
    /// branch is `default_branch`.
    #[must_use]
    pub fn for_base(base_ref: &str, default_branch: &str) -> Self {
        if base_ref == default_branch {
            Self("assay / verdict".to_owned())
        } else {
            Self(format!("assay / verdict @{base_ref}"))
        }
    }

    /// The name as GitHub shows it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CheckName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A check run's numeric id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckRunId(pub NonZeroU64);

/// An issue comment's numeric id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommentId(pub NonZeroU64);

/// The App's verdict check as GitHub shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    /// Its id.
    pub id: CheckRunId,
    /// `pending:<run_id>:<seq>` as `open` stamped it, or the decision key `decide` wrote.
    pub external_id: String,
    /// Its status.
    pub status: CheckStatus,
}

/// A check run's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckStatus {
    /// Queued.
    Queued,
    /// In progress: `open` stamped it, or a decision is pending.
    InProgress,
    /// Completed with a final conclusion.
    Completed,
}

/// Everything `decide` writes to the verdict check. There is no annotations field: location
/// data travels in inline comments and the per-gate checks (R3-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckUpdate {
    /// Status and conclusion.
    pub state: CheckState,
    /// `output.title`.
    pub title: String,
    /// `output.summary`: counts, reasons, tier, degraded roles, AEU and how to respond.
    pub summary: String,
    /// `output.text`: the ledger state it projects.
    pub text: String,
    /// The decision key (02 §5).
    pub external_id: String,
}

/// The status and conclusion `decide` gives the verdict check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckState {
    /// A `pending` decision (rule 8a), shown as `in_progress`, never `neutral` (03 §5).
    InProgress,
    /// Completed with this conclusion.
    Completed(CheckConclusion),
}

/// A final conclusion of the verdict check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckConclusion {
    /// `success`.
    Success,
    /// `failure`.
    Failure,
    /// `neutral`: an owner override.
    Neutral,
}

/// The App's summary comment on a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryComment {
    /// Its id.
    pub id: CommentId,
    /// Its body as GitHub returns it.
    pub body: String,
}

/// What `decide` records in the PR's ledger state for one evidence bundle (02 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceEntry {
    /// The repository's numeric id.
    pub repo_id: NonZeroU64,
    /// The PR number.
    pub pr: NonZeroU64,
    /// Where the bundle is: replay and decide-only read it only through this (R3-08).
    pub pointer: EvidencePointer,
    /// What the bundle holds, for `admit`'s reuse check (R3-17).
    pub record: EvidenceRecord,
}

/// The evidence pointer `{run_id, run_attempt, sha256}` (02 §5, R3-08).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidencePointer {
    /// The workflow run that uploaded the bundle.
    pub run_id: NonZeroU64,
    /// Its attempt, always 1.
    pub run_attempt: NonZeroU64,
    /// The sha256 of the uploaded bundle file.
    pub sha256: Sha256Digest,
}

/// The evidence record `{evidence_key, bundle_sha256, llm, tier}` (02 §5, R3-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRecord {
    /// The evidence key the bundle's run was planned under.
    pub evidence_key: String,
    /// The sha256 of the bundle, as in the pointer.
    pub bundle_sha256: Sha256Digest,
    /// How the LLM stage ran.
    pub llm: LlmEvidence,
    /// The tier the LLM stage ran at.
    pub tier: Tier,
}

/// How the LLM stage of an evidence bundle ran: only `Complete` can stand in for an LLM review
/// (R3-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LlmEvidence {
    /// It completed with no degraded role, or the tier has no LLM stage.
    Complete,
    /// It did not run because the PR was a draft (gates only).
    SkippedDraft,
    /// It was deferred, or did not run when it should have.
    Deferred,
    /// It completed with a degraded role.
    Degraded,
}

impl LlmEvidence {
    /// The record's wire word: `complete`, `skipped(draft)`, `deferred`, `degraded`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::SkippedDraft => "skipped(draft)",
            Self::Deferred => "deferred",
            Self::Degraded => "degraded",
        }
    }
}
