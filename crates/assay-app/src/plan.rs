//! The `plan` use case (S1, `docs/architecture/04-pipeline.md` §2): the run description
//! `assay.run/1` (`docs/architecture/03-contracts.md` §4) of one PR head.
//!
//! It reads base `.assay/` at the pinned `base_sha`, never at head; lists the changed files
//! with git over `base_sha...head_sha` and asserts them against GitHub's REST list; tiers the
//! change; names each job's artifact with its own nonce; and validates the document before
//! returning it. It has no ledger port: `admit` owns the ledger. Anything it cannot read or
//! reconcile is refused, loudly, rather than modelled (wi-b5affa).

mod base_config;
mod changes;
mod document;
mod nonce;
mod objects;
mod rest;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use assay_domain::{ContentHash, RunUid, Tiering, tier};
use thiserror::Error;

pub use self::base_config::BaseConfigError;
pub use self::document::{ArtifactJob, EvidenceKey};
pub use self::nonce::{ArtifactNonce, NonceSource, ParseNonceError};
pub use self::objects::{ChangedFiles, EntryKind, PullRequestObjects, TreeEntry};
pub use self::rest::{FileListError, REST_FILE_CAP, RestFile, RestFiles};
use crate::config::CommitId;
use crate::contract::{Contract, ContractValidator, ReadContractError};
use crate::tiering::PackageSet;

/// The PR as the triggering event names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestEvent {
    /// The repository, `owner/name`.
    pub repo: String,
    /// The PR number.
    pub number: NonZeroU64,
    /// The base commit the run is pinned to; base config is read here.
    pub base_sha: CommitId,
    /// The head commit the run is pinned to.
    pub head_sha: CommitId,
    /// The base branch, which `decide` re-fetches (R3-03).
    pub base_ref: String,
    /// Whether the PR is a draft: gates only (04 §2.4).
    pub draft: bool,
}

/// Which files `.rs` sources read at compile time (#64's `syn` scan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncludeTargets {
    /// The scan ran: these canonical repo-relative paths, at base or head.
    Scanned(Vec<String>),
    /// No scan ran: every changed path may be a target, so no change is `trivial`.
    NotScanned,
}

/// Everything `plan` is given besides the PR's objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRequest {
    /// The PR and its pinned commits.
    pub pull_request: PullRequestEvent,
    /// This workflow run; it keys leases and ledger records, never evidence reuse.
    pub run_uid: RunUid,
    /// GitHub's list of the PR's files, which git's must match.
    pub rest_files: RestFiles,
    /// The base commit's package set (#65); `None` when it could not be computed, which is
    /// exec surface.
    pub base_packages: Option<PackageSet>,
    /// The head commit's package set; `None` as for the base.
    pub head_packages: Option<PackageSet>,
    /// The compile-time include targets.
    pub include_targets: IncludeTargets,
    /// The reviewer prompt bundle's hash (04 §3), part of the evidence key.
    pub prompt_bundle: ContentHash,
}

/// A validated `assay.run/1` document and what it was computed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRun {
    json: Vec<u8>,
    run_key: EvidenceKey,
    tiering: Tiering,
    artifact_names: BTreeMap<ArtifactJob, String>,
}

impl PlannedRun {
    /// The document, exactly as validated against `run-1.schema.json`.
    #[must_use]
    pub fn json(&self) -> &[u8] {
        &self.json
    }

    /// The evidence key the document carries as `run_key`: for evidence reuse only.
    #[must_use]
    pub const fn run_key(&self) -> EvidenceKey {
        self.run_key
    }

    /// The tiering the document records.
    #[must_use]
    pub const fn tiering(&self) -> &Tiering {
        &self.tiering
    }

    /// Each job's artifact name.
    #[must_use]
    pub const fn artifact_names(&self) -> &BTreeMap<ArtifactJob, String> {
        &self.artifact_names
    }
}

/// Which call to the objects port failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectsStep {
    /// [`PullRequestObjects::changed_files`].
    ChangedFiles,
    /// [`PullRequestObjects::list`] at `base_sha`.
    ListBase,
    /// [`PullRequestObjects::list`] at `head_sha`.
    ListHead,
    /// [`PullRequestObjects::read`].
    Read,
}

impl ObjectsStep {
    /// The step as the error message names it.
    const fn describe(self) -> &'static str {
        match self {
            Self::ChangedFiles => "list the changed files",
            Self::ListBase => "list the base tree",
            Self::ListHead => "list the head tree",
            Self::Read => "read blobs",
        }
    }
}

/// Why no run description was written. Every variant fails the run.
#[derive(Debug, Error)]
pub enum PlanError<O, N> {
    /// The PR's objects could not be read.
    #[error("could not {} of the pull request", step.describe())]
    Objects {
        /// The port call that failed.
        step: ObjectsStep,
        /// The port's error.
        #[source]
        source: O,
    },
    /// The objects port returned a different number of blobs than entries asked for.
    #[error("asked for {asked} blobs, read {returned}")]
    ShortRead {
        /// Entries asked for.
        asked: usize,
        /// Blobs returned.
        returned: usize,
    },
    /// git did not list the changed files within its cap: there is no file list to plan, and
    /// the PR needs a split.
    #[error("git did not list the changed files within its cap; the PR needs a split")]
    DiffTooSlow,
    /// The git and REST file lists disagree, or the REST list is not usable.
    #[error("the PR's file list could not be confirmed")]
    FileList(#[source] FileListError),
    /// Base `.assay/` was refused.
    #[error("the base configuration was refused")]
    BaseConfig(#[source] BaseConfigError),
    /// No nonce could be drawn.
    #[error("could not draw an artifact nonce")]
    Nonce(#[source] N),
    /// The nonce source repeated itself, so artifact names would not be unpredictable.
    #[error("the nonce source repeated a nonce, for the {} artifact", job.name())]
    RepeatedNonce {
        /// The job whose nonce was a repeat.
        job: ArtifactJob,
    },
    /// The document could not be encoded.
    #[error("could not encode the run description")]
    Encode(#[source] serde_json::Error),
    /// The document failed its own schema: a defect in `plan`, never written out.
    #[error("the run description failed its schema")]
    Invalid(#[source] ReadContractError),
}

impl<O, N> PlanError<O, N> {
    /// Wraps a port error from `step`, for `map_err`.
    const fn objects(step: ObjectsStep) -> impl FnOnce(O) -> Self {
        move |source| Self::Objects { step, source }
    }
}

/// Plans one PR head: `request`'s PR read through `objects`, artifact nonces from `nonces`,
/// and the document checked by `validator` before it is returned.
///
/// # Examples
///
/// ```
/// use assay_app::{
///     ContractValidator, NonceSource, PlanError, PlanRequest, PullRequestObjects, plan,
/// };
///
/// # fn demo<O: PullRequestObjects, N: NonceSource>(
/// #     request: &PlanRequest,
/// #     objects: &O,
/// #     nonces: &mut N,
/// #     validator: &ContractValidator,
/// # ) -> Result<(), PlanError<O::Error, N::Error>> {
/// let run = plan(request, objects, nonces, validator)?;
/// // The bytes to upload as the `plan` artifact; its digest goes in the job's outputs.
/// let document: &[u8] = run.json();
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// [`PlanError`]: the objects could not be read ([`PlanError::Objects`],
/// [`PlanError::ShortRead`]), git ran past its cap ([`PlanError::DiffTooSlow`]), the file lists
/// disagree ([`PlanError::FileList`]), base `.assay/` was refused ([`PlanError::BaseConfig`]),
/// no fresh nonce could be drawn ([`PlanError::Nonce`], [`PlanError::RepeatedNonce`]), or the
/// document could not be written valid ([`PlanError::Encode`], [`PlanError::Invalid`]).
pub fn plan<O: PullRequestObjects, N: NonceSource>(
    request: &PlanRequest,
    objects: &O,
    nonces: &mut N,
    validator: &ContractValidator,
) -> Result<PlannedRun, PlanError<O::Error, N::Error>> {
    let pull_request = &request.pull_request;
    let files = match objects
        .changed_files(&pull_request.base_sha, &pull_request.head_sha)
        .map_err(PlanError::objects(ObjectsStep::ChangedFiles))?
    {
        ChangedFiles::Listed(files) => files,
        ChangedFiles::TooSlow => return Err(PlanError::DiffTooSlow),
    };
    rest::check(&request.rest_files, &files).map_err(PlanError::FileList)?;
    let config = base_config::read(objects, &pull_request.base_sha)?;
    let change_set = changes::change_set(objects, request, files)?;
    let tiering = tier(&config.globs, &change_set);
    let artifact_names = artifact_names(nonces)?;
    let run_key = EvidenceKey::new(pull_request, &config, request.prompt_bundle);
    let json = document::to_json(&document::Inputs {
        pull_request,
        run_uid: request.run_uid,
        run_key,
        config: &config,
        tiering: &tiering,
        files: &change_set.files,
        artifact_names: &artifact_names,
    })
    .map_err(PlanError::Encode)?;
    validator
        .read(Contract::Run, &json)
        .map_err(PlanError::Invalid)?;
    Ok(PlannedRun {
        json,
        run_key,
        tiering,
        artifact_names,
    })
}

/// One fresh nonce per job, named `assay-<job>-<nonce>`.
fn artifact_names<N: NonceSource, O>(
    nonces: &mut N,
) -> Result<BTreeMap<ArtifactJob, String>, PlanError<O, N::Error>> {
    let mut seen = BTreeSet::new();
    let mut names = BTreeMap::new();
    for job in ArtifactJob::ALL {
        let nonce = nonces.nonce().map_err(PlanError::Nonce)?;
        let name = format!("assay-{}-{nonce}", job.name());
        if !seen.insert(nonce) {
            return Err(PlanError::RepeatedNonce { job });
        }
        names.insert(job, name);
    }
    Ok(names)
}

/// Reads `entries` through `objects`, refusing a reply that is not one blob per entry.
fn read_entries<O: PullRequestObjects, N>(
    objects: &O,
    entries: &[&TreeEntry],
) -> Result<Vec<Vec<u8>>, PlanError<O::Error, N>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let blobs = objects
        .read(entries)
        .map_err(PlanError::objects(ObjectsStep::Read))?;
    if blobs.len() == entries.len() {
        Ok(blobs)
    } else {
        Err(PlanError::ShortRead {
            asked: entries.len(),
            returned: blobs.len(),
        })
    }
}
