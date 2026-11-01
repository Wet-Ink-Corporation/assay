//! The pipeline's `plan` job (#60 ST-5, S1, 04 §2): guard's identity planned over the PR's
//! objects into `<out>/run.json`, with plan's outputs (03 §4a) read off what was written.
//!
//! The binary fetches the pinned commits and GitHub's file list first; [`run_plan`] does the
//! rest over them, so it is tested against fixture repositories.

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;

use assay_app::{
    ContractValidator, IncludeTargets, NonceSource, PlanError, PlanRequest, PullRequestEvent,
    RestFiles, Sha256Digest, plan,
};
use assay_domain::ContentHash;

use crate::ci::{OutDir, RunEnv};
use crate::outputs::{Identity, PlanOutputs};
use crate::pull_request::{ForgeObjectsError, ForgePullRequest};

/// The run description's file name in `--out`, the plan artifact's only file.
pub const RUN_FILE: &str = "run.json";

/// What `plan` is given besides its ports.
#[derive(Debug)]
pub struct PlanJob<'a> {
    /// The run.
    pub run: &'a RunEnv,
    /// guard's identity of the PR (I2), moved into the plan request.
    pub identity: Identity,
    /// GitHub's list of the PR's files, which git's must match.
    pub rest_files: RestFiles,
    /// The embedded prompt bundle's hash, part of the evidence key.
    pub prompt_bundle: ContentHash,
}

/// What `plan` reads through, borrowed for one call.
#[derive(Debug)]
pub struct PlanPorts<'p, 'r, N> {
    /// The PR's objects, with the pinned base and head fetched.
    pub objects: &'p ForgePullRequest<'r>,
    /// The artifact nonces.
    pub nonces: &'p mut N,
    /// The contract validator the document is checked with.
    pub validator: &'p ContractValidator,
}

/// Why `plan` wrote no outputs. Every variant is exit 2: decide then refuses the run.
#[derive(Debug)]
pub enum PlanRunError<N> {
    /// The use case refused the run, `PlanError::FileList` for a REST list git does not
    /// confirm among them.
    Plan(PlanError<ForgeObjectsError, N>),
    /// git could not give the merge base.
    MergeBase(ForgeObjectsError),
    /// The pinned commits share no history, so there is no merge base.
    NoMergeBase,
    /// `--out` or the document in it could not be written.
    Write {
        /// The directory or file.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
}

impl<N> fmt::Display for PlanRunError<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(_) => f.write_str("the run could not be planned"),
            Self::MergeBase(_) => f.write_str("git could not give the merge base"),
            Self::NoMergeBase => f.write_str("the base and head commits share no history"),
            Self::Write { path, .. } => write!(f, "cannot write {}", path.display()),
        }
    }
}

impl<N: Error + 'static> Error for PlanRunError<N> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Plan(source) => Some(source),
            Self::MergeBase(source) => Some(source),
            Self::NoMergeBase => None,
            Self::Write { source, .. } => Some(source),
        }
    }
}

/// Plans `job`'s PR through `ports`, creates `out` and writes the document into it as
/// [`RUN_FILE`], and returns plan's outputs for what it wrote.
///
/// The request has no package sets (D2 B: every Rust change is exec surface, so `full`) and no
/// include-target scan, as local mode's; `decide` must build the same request.
///
/// # Errors
///
/// [`PlanRunError`]: the run was refused ([`PlanRunError::Plan`]), the merge base could not
/// be read ([`PlanRunError::MergeBase`], [`PlanRunError::NoMergeBase`]), or `out` or the
/// document could not be written ([`PlanRunError::Write`]; nothing is written before the
/// document is ready, and an `out` that appeared meanwhile is refused).
pub fn run_plan<N: NonceSource>(
    job: PlanJob<'_>,
    ports: PlanPorts<'_, '_, N>,
    out: &OutDir,
) -> Result<PlanOutputs, PlanRunError<N::Error>> {
    let PlanJob {
        run,
        identity,
        rest_files,
        prompt_bundle,
    } = job;
    let PlanPorts {
        objects,
        nonces,
        validator,
    } = ports;
    let request = plan_request(run, identity, rest_files, prompt_bundle);
    let planned = plan(&request, objects, nonces, validator).map_err(PlanRunError::Plan)?;
    let event = &request.pull_request;
    let merge_base = objects
        .merge_base(&event.base_sha, &event.head_sha)
        .map_err(PlanRunError::MergeBase)?
        .ok_or(PlanRunError::NoMergeBase)?;
    write_document(out, planned.json())?;
    let digest = Sha256Digest::of(planned.json());
    Ok(PlanOutputs::new(&planned, digest, event.draft, merge_base))
}

/// The plan request for `identity`'s PR in `run`: what `plan` plans and `decide` recomputes,
/// built here only, so the two cannot drift (#60 Risks). GitHub's file list is `rest_files`
/// and the prompt bundle's hash `prompt_bundle`; there are no package sets (D2 B) and no
/// include-target scan, as local mode's.
#[must_use]
pub(crate) fn plan_request(
    run: &RunEnv,
    identity: Identity,
    rest_files: RestFiles,
    prompt_bundle: ContentHash,
) -> PlanRequest {
    PlanRequest {
        pull_request: pull_request_event(run, identity),
        run_uid: run.run_uid,
        rest_files,
        base_packages: None,
        head_packages: None,
        include_targets: IncludeTargets::NotScanned,
        prompt_bundle,
    }
}

/// `identity`'s PR in `run`'s repository, as the run was planned for it.
#[must_use]
pub(crate) fn pull_request_event(run: &RunEnv, identity: Identity) -> PullRequestEvent {
    PullRequestEvent {
        repo: run.repo.to_string(),
        number: identity.pr,
        base_sha: identity.base_sha,
        head_sha: identity.head_sha,
        base_ref: identity.base_ref,
        draft: identity.draft,
    }
}

/// Creates `out` (refusing one that exists by now) and writes `json` as its only file.
fn write_document<N>(out: &OutDir, json: &[u8]) -> Result<(), PlanRunError<N>> {
    let failed = |path: PathBuf| move |source| PlanRunError::Write { path, source };
    fs::create_dir(out.path()).map_err(failed(out.path().to_path_buf()))?;
    let path = out.path().join(RUN_FILE);
    File::create_new(&path)
        .and_then(|mut file| file.write_all(json).and_then(|()| file.flush()))
        .map_err(failed(path))
}
