//! The ports `decide` publishes through: the verdict check and summary comment on GitHub, the
//! per-head lock and per-PR state in the ledger, and the run's artifact store (02 §5, 06 §1).
//!
//! Every forge call made while holding the per-head lock carries the lock lease's [`Deadline`]:
//! the adapter sends no request, and waits for no retry, that would not end before it (#290,
//! #332).

use std::fmt;
use std::num::NonZeroU64;

use assay_domain::{Side, Tier};

use crate::config::{CommitId, GateName, Sha256Digest};
use crate::forge::{Deadline, LockedWrite};
use crate::ledger::{DecisionPointer, PublicationRegister};

/// The verdict check and the summary comment, written with the `assay-verdict` App's token.
/// Implemented by the forge adapter (#53).
pub trait VerdictForge {
    /// Why GitHub could not be read or written.
    type Error: std::error::Error;

    /// The check run named `name` on `head` that the `assay-verdict` App authored, if any. A
    /// same-named check from any other app is never returned.
    ///
    /// Under the lock, `deadline` is the lease's: nothing is sent, and no retry waited for, that
    /// would not end before it. `None` only for `decide`'s first read, before the lock is taken.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the check runs cannot be listed, or the next request or wait would
    /// not end before the deadline (it is not made).
    fn find_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        deadline: Option<Deadline>,
    ) -> Result<Option<CheckRun>, Self::Error>;

    /// Overwrites the check's status, conclusion, output and `external_id` with `write.update`,
    /// sending nothing that would not end before `write.deadline`, the lock lease's. The update
    /// carries no annotations, so none ever accumulate on the verdict check (R3-11).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the check cannot be updated, or the next request or wait would not
    /// end before the deadline.
    fn update_check(&self, id: CheckRunId, write: LockedWrite<'_>) -> Result<(), Self::Error>;

    /// The comment on PR `number` that the App authored and whose body contains `marker`, if
    /// any. A comment by anyone else is never returned, whatever it contains. Nothing is sent
    /// that would not end before `deadline`, the lock lease's.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comments cannot be listed, or the next request or wait would not
    /// end before the deadline.
    fn find_summary(
        &self,
        number: NonZeroU64,
        marker: &str,
        deadline: Deadline,
    ) -> Result<Option<SummaryComment>, Self::Error>;

    /// Posts `body` as a new comment on PR `number`, sending nothing that would not end before
    /// `deadline`, the lock lease's.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comment cannot be posted, or the next request or wait would not
    /// end before the deadline.
    fn create_summary(
        &self,
        number: NonZeroU64,
        body: &str,
        deadline: Deadline,
    ) -> Result<(), Self::Error>;

    /// Replaces the body of comment `id` with `body`, sending nothing that would not end before
    /// `deadline`, the lock lease's.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comment cannot be edited, or the next request or wait would not
    /// end before the deadline.
    fn update_summary(
        &self,
        id: CommentId,
        body: &str,
        deadline: Deadline,
    ) -> Result<(), Self::Error>;
}

/// The inline review comments and the per-gate checks, written with the `assay-verdict` App's
/// token beside the verdict check, under the per-head lock. Implemented by the forge adapter
/// (#53).
pub trait CommentForge: VerdictForge {
    /// The bodies of every review comment on PR `number` that the App authored, across all
    /// pages. A comment by anyone else is never returned, whatever it contains, so no one else
    /// can mark a fingerprint as posted. Nothing is sent that would not end before `deadline`,
    /// the lock lease's.
    ///
    /// # Errors
    ///
    /// [`VerdictForge::Error`] when the review comments cannot be listed, or the next request
    /// or wait would not end before the deadline.
    fn app_review_comments(
        &self,
        number: NonZeroU64,
        deadline: Deadline,
    ) -> Result<Vec<String>, Self::Error>;

    /// Posts `comment` on the diff of PR `number`, as a single review comment, sending nothing
    /// that would not end before `deadline`, the lock lease's.
    ///
    /// # Errors
    ///
    /// [`VerdictForge::Error`] when the comment cannot be posted, or the next request or wait
    /// would not end before the deadline.
    fn create_review_comment(
        &self,
        number: NonZeroU64,
        comment: &InlineComment<'_>,
        deadline: Deadline,
    ) -> Result<(), Self::Error>;

    /// Creates the check run `name` on `head` with `write.update`'s status, conclusion, output
    /// and `external_id`, sending nothing that would not end before `write.deadline`, the lock
    /// lease's. Never called for the verdict check, which only `open` creates.
    ///
    /// # Errors
    ///
    /// [`VerdictForge::Error`] when the check cannot be created, or the next request or wait
    /// would not end before the deadline.
    fn create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        write: LockedWrite<'_>,
    ) -> Result<(), Self::Error>;
}

/// One inline review comment: where on the diff it goes and its body, already sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineComment<'a> {
    /// The head commit the comment is anchored at.
    pub commit_id: &'a CommitId,
    /// The file's path at head (the base path for a deleted file).
    pub path: &'a str,
    /// The last line of the span, as GitHub's `line`.
    pub line: NonZeroU64,
    /// The first line of a span over several lines, as GitHub's `start_line`.
    pub start_line: Option<NonZeroU64>,
    /// The side of the diff the lines are on.
    pub side: Side,
    /// The Markdown body, with the hidden fingerprint marker on its first line.
    pub body: String,
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
    fn acquire(
        &self,
        repo_id: NonZeroU64,
        head: &CommitId,
    ) -> Result<Option<Self::Lease>, Self::Error>;

    /// Releases the lock `lease` holds, by compare-and-set: a lock another writer took after this
    /// one expired is never released.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the lock could not be released; it then expires with its TTL.
    fn release(&self, lease: Self::Lease) -> Result<(), Self::Error>;

    /// The deadline of every call made while holding `lease`: its `expires_at`, after which
    /// another writer may reap the lock (#290, #332).
    fn deadline(&self, lease: &Self::Lease) -> Deadline;

    /// Writes `entry`'s evidence pointer and evidence record into the PR's state
    /// `state/<repo_id>/<pr>.json`, by compare-and-set on the file, while holding `lease`. The
    /// write starts only before the lease's [`deadline`](Self::deadline).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the state file could not be written, or the lease's deadline has
    /// come (nothing is written).
    fn record_evidence(
        &self,
        lease: &Self::Lease,
        entry: &EvidenceEntry,
    ) -> Result<(), Self::Error>;

    /// Writes `entry`'s publication register into the PR's state while holding `lease`, by
    /// compare-and-set on the file (D2 of #61): over whatever was there for a pipeline run
    /// (`over` is `None`), or only while the state still holds `over`'s register and evidence
    /// pointer. The write starts only before the lease's [`deadline`](Self::deadline).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the state file could not be written, the register names another
    /// head than the lock's or another run than the writer, or the lease's deadline has come
    /// (nothing is written).
    fn record_published(
        &self,
        lease: &Self::Lease,
        entry: &PublishedEntry,
    ) -> Result<Claimed, Self::Error>;

    /// Writes `entry`'s decision pointer and publication register into the PR's state in one
    /// compare-and-set while holding `lease`, only while the state still holds `entry.over`'s
    /// register and evidence pointer (decide-only, D2 of #61). The write starts only before the
    /// lease's [`deadline`](Self::deadline).
    ///
    /// # Errors
    ///
    /// As [`record_published`](Self::record_published), and when the pointer names another run
    /// than the writer.
    fn record_decision(
        &self,
        lease: &Self::Lease,
        entry: &DecisionEntry,
    ) -> Result<Claimed, Self::Error>;
}

/// What decide-only read of a PR's state before it re-decided: the register it claims the
/// check by and the evidence pointer it re-decided from (D2 of #61).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The publication register: decide-only writes the check only while it carries its key.
    pub register: PublicationRegister,
    /// The evidence pointer the re-decision's evidence bundle was read through.
    pub evidence: EvidencePointer,
}

/// A publication register to record, and the claim it replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedEntry {
    /// The repository's numeric id.
    pub repo_id: NonZeroU64,
    /// The PR number.
    pub pr: NonZeroU64,
    /// The register: the head, the key the check is written under and the writing run.
    pub register: PublicationRegister,
    /// `None` for a pipeline run, which owns the check by its stamp; decide-only's claim, which
    /// the state must still hold, otherwise.
    pub over: Option<Claim>,
}

/// decide-only's decision pointer and publication register, recorded together (J10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionEntry {
    /// The repository's numeric id.
    pub repo_id: NonZeroU64,
    /// The PR number.
    pub pr: NonZeroU64,
    /// Where the decision bundle is: written only once its upload is confirmed.
    pub pointer: DecisionPointer,
    /// The register: the head, the decision key and the decide-only run.
    pub register: PublicationRegister,
    /// The claim the state must still hold.
    pub over: Claim,
}

/// Whether a claimed register was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Claimed {
    /// It was recorded (or was already the state's).
    Recorded,
    /// The state no longer holds the claim's register or evidence pointer: another writer
    /// published since; nothing was written.
    Moved,
}

/// The run's artifact store, written with the run's token. Implemented by the forge adapter.
pub trait BundleStore {
    /// Why the artifact could not be uploaded.
    type Error: std::error::Error;

    /// Uploads `bytes` as `file` of the artifact named `artifact`, kept 90 days.
    ///
    /// Idempotent for equal bytes: when `artifact` already holds `file` with exactly `bytes`
    /// (the same run publishing again after `open` re-stamped its check), the upload succeeds
    /// without writing, so a republish never turns a persisted decision into `persist-error`.
    /// An existing `file` with other bytes is never overwritten: that is an error, since the
    /// evidence pointer already names the first upload's digest.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the upload failed, or `artifact` already holds `file` with other
    /// bytes.
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

    /// The name of the non-required check for plugin `gate`: `assay / gate: <name>`.
    pub(crate) fn for_gate(gate: &GateName) -> Self {
        Self(format!("assay / gate: {gate}"))
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
    /// `cancelled`: written only by the reconcile, on a superseded head's check (06 §4).
    Cancelled,
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
