//! The workflow checks, over the files of the default branch's `.github/workflows`: no
//! installed assay workflow runs from a PR merge commit (T20), every action of every workflow
//! is pinned by full commit SHA, and every privileged installed job carries the
//! `github.run_attempt == 1` guard (T22, R3-05).
//!
//! The installed assay workflows are the `assay*.yml` and `assay*.yaml` files (the templates
//! `assay.yml` and `assay-decide.yml`, 02 §7); with none installed, the two installed-workflow
//! checks fail.

use std::path::Path;

use super::port::{EntryType, PrerequisiteForge};
use super::report::PrerequisiteFailure;
use super::workflow::{Workflow, is_pinned};
use super::{Judged, Scene};

/// Where GitHub runs workflows from.
const WORKFLOWS: &str = ".github/workflows";

/// The file-name prefix of the installed assay workflows.
const INSTALLED_PREFIX: &str = "assay";

/// Events whose workflow file GitHub takes from the PR merge commit (T20).
const MERGE_COMMIT_EVENTS: [&str; 3] = [
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
];

/// Which workflow files a check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Files {
    /// Every workflow.
    All,
    /// The installed assay workflows.
    Installed,
}

/// The workflow files `files` selects, read, with their paths.
fn read_workflows<F: PrerequisiteForge>(
    scene: &Scene<'_, F>,
    files: Files,
) -> Result<Vec<(String, Workflow)>, PrerequisiteFailure<F::Error>> {
    let forge = scene.repository;
    let branch = &scene.base.default_branch;
    let entries = forge
        .directory(branch, WORKFLOWS)
        .map_err(PrerequisiteFailure::Unreadable)?
        // No directory is no workflow.
        .unwrap_or_default();
    let mut read = Vec::new();
    for entry in entries {
        let name = entry.name.as_str();
        // In any ASCII case: reading more files can only fail more checks (wi-b5affa).
        let workflow_file = Path::new(name).extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("yml") || extension.eq_ignore_ascii_case("yaml")
        });
        let installed = name
            .get(..INSTALLED_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(INSTALLED_PREFIX));
        if !workflow_file || (files == Files::Installed && !installed) {
            continue;
        }
        let path = format!("{WORKFLOWS}/{name}");
        if entry.kind != EntryType::File {
            return Err(PrerequisiteFailure::WorkflowNotAFile { path });
        }
        let Some(bytes) = forge
            .file(branch, &path)
            .map_err(PrerequisiteFailure::Unreadable)?
        else {
            return Err(PrerequisiteFailure::WorkflowVanished { path });
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return Err(PrerequisiteFailure::WorkflowNotUtf8 { path });
        };
        match Workflow::read(&text) {
            Ok(workflow) => read.push((path, workflow)),
            Err(source) => return Err(PrerequisiteFailure::Workflow { path, source }),
        }
    }
    if files == Files::Installed && read.is_empty() {
        return Err(PrerequisiteFailure::NoWorkflows);
    }
    Ok(read)
}

/// No installed workflow declares an event that runs the PR merge commit's file.
pub(super) fn merge_commit_trigger<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    for (path, workflow) in read_workflows(scene, Files::Installed)? {
        if let Some(event) = workflow
            .triggers
            .into_iter()
            .find(|event| MERGE_COMMIT_EVENTS.contains(&event.as_str()))
        {
            return Err(PrerequisiteFailure::MergeCommitTrigger { path, event });
        }
    }
    Ok(())
}

/// Every `uses` of every workflow is pinned.
pub(super) fn pinned_actions<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    for (path, workflow) in read_workflows(scene, Files::All)? {
        if let Some(uses) = workflow.uses.into_iter().find(|uses| !is_pinned(uses)) {
            return Err(PrerequisiteFailure::Unpinned { path, uses });
        }
    }
    Ok(())
}

/// Every installed job holding a secret or a write token is guarded.
pub(super) fn run_attempt_guard<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    for (path, workflow) in read_workflows(scene, Files::Installed)? {
        for job in workflow.jobs {
            if let (Some(holds), false) = (job.holds, job.guarded) {
                return Err(PrerequisiteFailure::Unguarded {
                    path,
                    job: job.id,
                    holds,
                });
            }
        }
    }
    Ok(())
}
