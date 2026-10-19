//! The workflow checks, over the files of the default branch's `.github/workflows`: every
//! installed assay workflow is triggered only by events that run the default branch's file
//! (T20), every action of every workflow is pinned by full commit SHA, and every privileged
//! installed job carries the `github.run_attempt == 1` guard (T22, R3-05).
//!
//! The installed assay workflows are the `assay*.yml` and `assay*.yaml` files (the templates
//! `assay.yml` and `assay-decide.yml`, 02 §7); with none installed, the two installed-workflow
//! checks fail. The files are read once per run ([`read_files`]); each check judges the ones
//! it selects.

use std::path::Path;

use super::port::{EntryType, PrerequisiteForge};
use super::report::PrerequisiteFailure;
use super::workflow::{Workflow, WorkflowError, is_pinned};
use super::{Judged, Scene};

/// Where GitHub runs workflows from.
const WORKFLOWS: &str = ".github/workflows";

/// The file-name prefix of the installed assay workflows.
const INSTALLED_PREFIX: &str = "assay";

/// The only events an installed assay workflow may declare: those GitHub runs from the default
/// branch's workflow file and that the spec gives assay's workflows (02 §1 and §5, 04 §6,
/// 05 T20): `pull_request_target` (the pipeline and decide-only's labels), `workflow_dispatch`
/// (the pipeline's evidence, R3-04), `issue_comment` (decide-only's commands) and `schedule`
/// (decide-only's poll, reconcile). Any other name, a misspelt or unknown one included, fails
/// (wi-b5affa).
const ALLOWED_EVENTS: [&str; 4] = [
    "pull_request_target",
    "workflow_dispatch",
    "issue_comment",
    "schedule",
];

/// One file of `.github/workflows` with a workflow extension, as read.
pub(super) struct WorkflowFile {
    path: String,
    installed: bool,
    read: Result<Workflow, FileProblem>,
}

/// Why a listed workflow file has no workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileProblem {
    /// The entry is not a regular file.
    NotAFile,
    /// The listed file could not be fetched.
    Vanished,
    /// The file is not UTF-8.
    NotUtf8,
    /// The file could not be classified.
    Refused(WorkflowError),
}

/// Which workflow files a check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Files {
    /// Every workflow.
    All,
    /// The installed assay workflows.
    Installed,
}

/// Reads every workflow file of `branch`'s `.github/workflows` once.
///
/// # Errors
///
/// The forge's error when the listing or a file cannot be read.
pub(super) fn read_files<F: PrerequisiteForge>(
    forge: &F,
    branch: &str,
) -> Result<Vec<WorkflowFile>, F::Error> {
    let entries = forge
        .directory(branch, WORKFLOWS)?
        // No directory is no workflow.
        .unwrap_or_default();
    let mut files = Vec::new();
    for entry in entries {
        let name = entry.name.as_str();
        // In any ASCII case: reading more files can only fail more checks (wi-b5affa).
        let workflow_file = Path::new(name).extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("yml") || extension.eq_ignore_ascii_case("yaml")
        });
        if !workflow_file {
            continue;
        }
        let installed = name
            .get(..INSTALLED_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(INSTALLED_PREFIX));
        let path = format!("{WORKFLOWS}/{name}");
        let read = if entry.kind == EntryType::File {
            match forge.file(branch, &path)?.map(String::from_utf8) {
                None => Err(FileProblem::Vanished),
                Some(Err(_)) => Err(FileProblem::NotUtf8),
                Some(Ok(text)) => Workflow::read(&text).map_err(FileProblem::Refused),
            }
        } else {
            Err(FileProblem::NotAFile)
        };
        files.push(WorkflowFile {
            path,
            installed,
            read,
        });
    }
    Ok(files)
}

/// The workflows `files` selects, with their paths; the first selected file without a
/// workflow fails the check.
fn workflows<'s, F: PrerequisiteForge>(
    scene: &'s Scene<'_, F>,
    files: Files,
) -> Result<Vec<(&'s str, &'s Workflow)>, PrerequisiteFailure<F::Error>> {
    let mut selected = Vec::new();
    for file in scene.workflows.get()? {
        if files == Files::Installed && !file.installed {
            continue;
        }
        // Each failure owns its path; the shared read keeps its own.
        let path = || file.path.clone();
        match &file.read {
            Ok(workflow) => selected.push((file.path.as_str(), workflow)),
            Err(FileProblem::NotAFile) => {
                return Err(PrerequisiteFailure::WorkflowNotAFile { path: path() });
            }
            Err(FileProblem::Vanished) => {
                return Err(PrerequisiteFailure::WorkflowVanished { path: path() });
            }
            Err(FileProblem::NotUtf8) => {
                return Err(PrerequisiteFailure::WorkflowNotUtf8 { path: path() });
            }
            Err(FileProblem::Refused(source)) => {
                return Err(PrerequisiteFailure::Workflow {
                    path: path(),
                    source: source.clone(),
                });
            }
        }
    }
    if files == Files::Installed && selected.is_empty() {
        return Err(PrerequisiteFailure::NoWorkflows);
    }
    Ok(selected)
}

// The failures below own their path, event, `uses` and job id; the shared read keeps its own,
// for the checks after them.

/// Every installed workflow declares only allowlisted events.
pub(super) fn merge_commit_trigger<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    for (path, workflow) in workflows(scene, Files::Installed)? {
        if let Some(event) = workflow
            .triggers
            .iter()
            .find(|event| !ALLOWED_EVENTS.contains(&event.as_str()))
        {
            return Err(PrerequisiteFailure::MergeCommitTrigger {
                path: path.to_owned(),
                event: event.clone(),
            });
        }
    }
    Ok(())
}

/// Every `uses` of every workflow is pinned.
pub(super) fn pinned_actions<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    for (path, workflow) in workflows(scene, Files::All)? {
        if let Some(uses) = workflow.uses.iter().find(|uses| !is_pinned(uses)) {
            return Err(PrerequisiteFailure::Unpinned {
                path: path.to_owned(),
                uses: uses.clone(),
            });
        }
    }
    Ok(())
}

/// Every installed job holding a secret or a write token is guarded.
pub(super) fn run_attempt_guard<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    for (path, workflow) in workflows(scene, Files::Installed)? {
        for job in &workflow.jobs {
            if let (Some(holds), false) = (job.holds, job.guarded) {
                return Err(PrerequisiteFailure::Unguarded {
                    path: path.to_owned(),
                    job: job.id.clone(),
                    holds,
                });
            }
        }
    }
    Ok(())
}
