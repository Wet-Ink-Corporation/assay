//! `assay check --local` (02 §6, R-10, P9): the pipeline's advisory verdict on a developer's
//! working tree, before anything is pushed.
//!
//! [`check_local`] runs `plan → gate → review → decide` over a snapshot of the working tree
//! through the narrow ports those stages already define, plus [`ChangePatches`] for the diff the
//! review reads. Nothing it produces is a contract: the [`LocalReport`] carries its own schema
//! name, which [`ContractValidator`] refuses for every contract, so no CI reader can accept it.
//! Local mode is never a trust anchor.

mod gates;
mod inputs;
mod known;
mod pack;
mod ports;
mod report;
mod review;

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::path::Path;
use std::time::Duration;

use assay_domain::{FileChange, Side, decide};
use thiserror::Error;

pub use self::gates::LocalGate;
use self::gates::Ran;
use self::inputs::Facts;
pub use self::pack::{MAX_DIFF_BYTES, MAX_FILE_LIST_BYTES};
pub use self::ports::{ChangePatches, FilePatch};
pub use self::report::{LocalReport, LocalReview, ReviewStatus};
use self::review::Reviewer;
use crate::config::CommitId;
use crate::contract::ContractValidator;
use crate::decide::{Cited, ReadFailure, Texts, policy, read_texts};
use crate::gate::GateRunner;
use crate::golden::parse_rubric;
use crate::model_runner::ModelRunner;
use crate::plan::{self, PlanError, PlanRequest, PullRequestObjects, TreeEntry};
use crate::prompt_bundle::PromptBundle;
use crate::validation::{Change, DiffPaths, EgressScan, FileDiff, KnownRuns};

/// What a local check is asked to check.
#[derive(Debug, Clone, Copy)]
pub struct CheckRequest<'a> {
    /// The working tree as a pull request: base is the remote branch's commit, head the
    /// snapshot commit, and the file lists git's own.
    pub plan: &'a PlanRequest,
    /// The tree fingerprint: the snapshot's git tree id (02 §6).
    pub tree: &'a str,
    /// The rubric file's bytes (`rubric/v1.toml`).
    pub rubric: &'a [u8],
    /// How the review is called.
    pub review: ReviewSetup<'a>,
}

/// How the review's one model call is made.
#[derive(Debug, Clone, Copy)]
pub struct ReviewSetup<'a> {
    /// The prompt bundle; its hash is in the plan request's evidence key.
    pub bundle: &'a PromptBundle,
    /// The bundle's system prompt file (absolute).
    pub system_prompt_file: &'a Path,
    /// A checkout of the snapshot the model may read (absolute): never the working tree
    /// itself, whose ignored files the model must not see.
    pub tree_dir: &'a Path,
    /// The wall-clock limit of each attempt.
    pub timeout: Duration,
}

/// The ports a local check runs through.
#[derive(Debug)]
pub struct CheckPorts<'p, O, P, G, M> {
    /// The snapshot's and base's objects.
    pub objects: &'p O,
    /// The change's diff.
    pub patches: &'p P,
    /// The gates that run locally; any other is `gate-error:not-run`.
    pub gates: &'p mut G,
    /// The model (the logged-in interactive CLI in production).
    pub model: &'p M,
}

/// Why no local report could be built. Every variant stops the check.
#[derive(Debug, Error)]
pub enum CheckError<O, P> {
    /// The rubric file does not parse.
    #[error("the rubric does not parse (line {line})")]
    Rubric {
        /// The 1-based line of the parse error (0 when it has none).
        line: usize,
    },
    /// `plan` refused the change.
    #[error("plan refused the change")]
    Plan(#[source] PlanError<O, Infallible>),
    /// The change's patches could not be read.
    #[error("could not read the change's diff")]
    Patches(#[source] P),
    /// The patches port returned another number of patches than files asked for.
    #[error("asked for {asked} patches, read {returned}")]
    PatchCount {
        /// Files asked for.
        asked: usize,
        /// Patches returned.
        returned: usize,
    },
    /// The changed, cited or snapshot files could not be read.
    #[error("could not read the changed files")]
    Objects(#[source] O),
    /// The objects port returned another number of blobs than entries asked for.
    #[error("asked for {asked} blobs, read {returned}")]
    ShortRead {
        /// Entries asked for.
        asked: usize,
        /// Blobs returned.
        returned: usize,
    },
    /// The review's document could not be encoded.
    #[error("could not encode the review's findings document")]
    Encode(#[source] serde_json::Error),
}

impl<O, P> From<ReadFailure<O>> for CheckError<O, P> {
    fn from(failure: ReadFailure<O>) -> Self {
        match failure {
            ReadFailure::Objects(source) => Self::Objects(source),
            ReadFailure::ShortRead { asked, returned } => Self::ShortRead { asked, returned },
        }
    }
}

/// Checks the working tree `request` describes as CI would check it as a PR, and decides.
///
/// It plans the change, runs every scheduled gate through `ports.gates`, reviews it with one
/// model call when its tier has an LLM stage, and decides with the rubric. The result is
/// advisory.
///
/// # Errors
///
/// [`CheckError`]: the rubric does not parse, `plan` refused the change, or the diff or the
/// changed files could not be read. A gate or model that fails is an outcome in the report, not
/// an error.
pub fn check_local<O, P, G, M>(
    request: &CheckRequest<'_>,
    ports: CheckPorts<'_, O, P, G, M>,
    validator: &ContractValidator,
) -> Result<LocalReport, CheckError<O::Error, P::Error>>
where
    O: PullRequestObjects,
    P: ChangePatches,
    G: GateRunner,
    M: ModelRunner,
{
    let CheckPorts {
        objects,
        patches,
        gates: runner,
        model,
    } = ports;
    let rubric = parse_rubric(request.rubric).map_err(|line| CheckError::Rubric { line })?;
    let event = &request.plan.pull_request;
    let commits = (&event.base_sha, &event.head_sha);
    let recomputed = plan::recompute(request.plan, objects).map_err(CheckError::Plan)?;
    let files = &recomputed.files;
    let patches = read_patches(patches, commits, files)?;
    let ran = gates::run(&recomputed.config.plugins, runner, validator);
    let role = review::role(recomputed.tiering.tier);
    let ReviewReads { paths, snapshot } = match role {
        Some(_) => review_reads(objects, commits, files)?,
        None => ReviewReads::default(),
    };
    let texts = read_texts(objects, commits, files, &cite(&ran, &paths))?;
    let gates = gates::anchor(ran, &texts);
    let (change, patch_texts) = change(texts, paths, patches);
    let reviewer = Reviewer {
        setup: &request.review,
        model,
        validator,
        run_key: recomputed.run_key,
    };
    let outcome = role
        .map(|role| {
            let pack = pack::context_pack(&change.diffs, &patch_texts);
            let egress = EgressScan::without_token(
                known_runs(&snapshot).with_commits([&event.base_sha, &event.head_sha]),
            );
            let inputs = review::inputs(event.repo.as_str(), &change, &gates.findings, &egress);
            let outcome = reviewer.review(role, &pack, &inputs)?;
            Ok((role, outcome))
        })
        .transpose()
        .map_err(CheckError::Encode)?;
    let facts = Facts {
        tree: request.tree,
        run_key: recomputed.run_key,
        policy: policy(recomputed.config.plugins.assay()),
        base_ref: event.base_ref.as_str(),
        tiering: &recomputed.tiering,
        plugins: &recomputed.config.plugins,
    };
    let decision = decide(&rubric, &facts.inputs(gates.reports, outcome.as_ref()));
    Ok(LocalReport {
        schema: report::SCHEMA,
        advisory: true,
        tree: request.tree.to_owned(),
        base: event.base_sha.as_str().to_owned(),
        head: event.head_sha.as_str().to_owned(),
        tier: recomputed.tiering.tier,
        gates: gates.local,
        review: report::review(outcome),
        decision,
    })
}

/// The patch of each of `files`, refusing a reply that is not one patch per file.
fn read_patches<P: ChangePatches, O>(
    patches: &P,
    (base, head): (&CommitId, &CommitId),
    files: &[FileChange],
) -> Result<Vec<FilePatch>, CheckError<O, P::Error>> {
    let read = patches
        .patches(base, head, files)
        .map_err(CheckError::Patches)?;
    if read.len() == files.len() {
        Ok(read)
    } else {
        Err(CheckError::PatchCount {
            asked: files.len(),
            returned: read.len(),
        })
    }
}

/// The change validation reads, with each file's hunks, and the patch texts for the pack.
fn change(texts: Texts, paths: Vec<DiffPaths>, patches: Vec<FilePatch>) -> (Change, Vec<String>) {
    let (head_files, base_files) = texts.into_sides();
    let (patch_texts, hunks): (Vec<String>, Vec<_>) = patches
        .into_iter()
        .map(|patch| (patch.text, patch.hunks))
        .unzip();
    let change = Change {
        head_files,
        base_files,
        diffs: paths
            .into_iter()
            .zip(hunks)
            .map(|(paths, hunks)| FileDiff { paths, hunks })
            .collect(),
    };
    (change, patch_texts)
}

/// What only a review reads.
#[derive(Debug, Default)]
struct ReviewReads {
    /// Each changed file's paths.
    paths: Vec<DiffPaths>,
    /// The snapshot's texts, for the egress scan.
    snapshot: Vec<Vec<u8>>,
}

/// Reads what a review needs, listing the head tree once for both.
fn review_reads<O: PullRequestObjects, P>(
    objects: &O,
    (base, head): (&CommitId, &CommitId),
    files: &[FileChange],
) -> Result<ReviewReads, CheckError<O::Error, P>> {
    let listed = objects.list(head, None).map_err(CheckError::Objects)?;
    Ok(ReviewReads {
        paths: diff_paths(objects, base, &listed, files)?,
        snapshot: known::snapshot_texts(objects, &listed)?,
    })
}

/// The long runs of the snapshot's `texts`, which [`known::snapshot_texts`] already holds under
/// [`crate::validation::MAX_INDEXED_BYTES`].
fn known_runs(texts: &[Vec<u8>]) -> KnownRuns<'_> {
    // `TooLarge` cannot happen while the texts are capped. Were it reached, the empty index
    // explains nothing, so every long run is a hit: the fail-closed fallback `KnownRuns::index`
    // documents, not a hidden failure.
    KnownRuns::index(texts.iter().map(Vec::as_slice)).unwrap_or_default()
}

/// Each changed file's paths, from whether it exists at `base` and in the `head` listing.
fn diff_paths<O: PullRequestObjects, P>(
    objects: &O,
    base: &CommitId,
    head: &[TreeEntry],
    files: &[FileChange],
) -> Result<Vec<DiffPaths>, CheckError<O::Error, P>> {
    let at_base: BTreeSet<String> = objects
        .list(base, None)
        .map_err(CheckError::Objects)?
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    let at_head: BTreeSet<&str> = head.iter().map(|entry| entry.path.as_str()).collect();
    Ok(files
        .iter()
        .map(|file| {
            let before = file.renamed_from.as_deref().unwrap_or(&file.path);
            // Each path is owned by the diff; the change keeps its own.
            match (
                &file.renamed_from,
                at_base.contains(before),
                at_head.contains(file.path.as_str()),
            ) {
                (Some(from), _, _) => DiffPaths::Renamed {
                    base: from.clone(),
                    head: file.path.clone(),
                },
                (None, false, true) => DiffPaths::Added {
                    head: file.path.clone(),
                },
                (None, true, false) => DiffPaths::Deleted {
                    base: file.path.clone(),
                },
                (None, _, _) => DiffPaths::Modified {
                    path: file.path.clone(),
                },
            }
        })
        .collect())
}

/// The files the gates' findings cite, and each changed file on each side it exists on.
fn cite<'a>(ran: &'a [Ran], paths: &'a [DiffPaths]) -> Cited<'a> {
    let mut cited = Cited::default();
    for gate in ran {
        gate.cite(&mut cited);
    }
    for path in paths {
        cite_both_sides(&mut cited, path);
    }
    cited
}

/// Cites `paths` on each side it exists on.
fn cite_both_sides<'p>(cited: &mut Cited<'p>, paths: &'p DiffPaths) {
    match paths {
        DiffPaths::Added { head } => cited.add_path(Side::Right, head),
        DiffPaths::Deleted { base } => cited.add_path(Side::Left, base),
        DiffPaths::Modified { path } => {
            cited.add_path(Side::Right, path);
            cited.add_path(Side::Left, path);
        }
        DiffPaths::Renamed { base, head } => {
            cited.add_path(Side::Right, head);
            cited.add_path(Side::Left, base);
        }
    }
}
