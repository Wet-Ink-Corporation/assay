//! The git file list asserted against GitHub's REST list (`docs/architecture/04-pipeline.md`
//! §2): the plan never trusts the REST list alone, and refuses when the two disagree.

use std::collections::BTreeSet;

use assay_domain::FileChange;
use thiserror::Error;

/// The most files `GET /repos/{owner}/{repo}/pulls/{number}/files` lists; past it the list is
/// truncated.
pub const REST_FILE_CAP: usize = 3_000;

/// GitHub's list of a PR's files, as the REST API reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestFiles {
    /// Every file listed, across all pages.
    pub files: Vec<RestFile>,
    /// The PR's `changed_files` count, which the list matches unless it is truncated.
    pub changed_files: u64,
}

/// One file of the REST list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestFile {
    /// `filename`.
    pub path: String,
    /// `previous_filename`, for a file GitHub reports as renamed.
    pub previous_path: Option<String>,
}

/// Why the git and REST file lists could not be reconciled. Every variant fails the plan.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FileListError {
    /// The REST list holds more files than the API ever returns.
    #[error("the REST list holds {listed} files, over the API's cap of {REST_FILE_CAP}")]
    OverCap {
        /// Files listed.
        listed: usize,
    },
    /// Below the cap, the REST list does not hold the PR's `changed_files`: a page is missing
    /// or the PR moved while it was read.
    #[error("the REST list holds {listed} files, but the PR changed {changed_files}")]
    Incomplete {
        /// Files listed.
        listed: usize,
        /// The PR's `changed_files`.
        changed_files: u64,
    },
    /// The REST list is truncated and git lists fewer paths than the PR changed files: git's
    /// list cannot be complete, and nothing else can confirm it.
    #[error("git lists {git_paths} paths, but the PR changed {changed_files} files")]
    GitShort {
        /// Distinct paths git lists, both sides of every rename.
        git_paths: usize,
        /// The PR's `changed_files`.
        changed_files: u64,
    },
    /// The REST list names one file twice.
    #[error("the REST list names a file twice")]
    Repeated,
    /// The lists name different files.
    #[error(
        "git and REST list different files: {only_in_git} only in git, {only_in_rest} only in \
         REST, first {first:?}"
    )]
    Mismatch {
        /// Paths only git lists (not counted for a truncated REST list).
        only_in_git: usize,
        /// Paths only the REST list names.
        only_in_rest: usize,
        /// The first differing path, in path order.
        first: String,
    },
}

/// Checks `rest` against the git list `git`, comparing every path either names on either side
/// of a rename (so a rename one side detected and the other read as a deletion and an
/// addition still matches). A complete REST list must name exactly git's paths; a truncated
/// one (`REST_FILE_CAP` listed, more changed) only paths git lists.
pub(super) fn check(rest: &RestFiles, git: &[FileChange]) -> Result<(), FileListError> {
    let listed = rest.files.len();
    if listed > REST_FILE_CAP {
        return Err(FileListError::OverCap { listed });
    }
    let named: BTreeSet<&str> = rest.files.iter().map(|file| file.path.as_str()).collect();
    if named.len() != listed {
        return Err(FileListError::Repeated);
    }
    // Both are at most `REST_FILE_CAP` here, so neither conversion can fail.
    let listed_count = u64::try_from(listed).unwrap_or(u64::MAX);
    let cap = u64::try_from(REST_FILE_CAP).unwrap_or(u64::MAX);
    let truncated = listed == REST_FILE_CAP && rest.changed_files > cap;
    if !truncated && rest.changed_files != listed_count {
        return Err(FileListError::Incomplete {
            listed,
            changed_files: rest.changed_files,
        });
    }
    let rest_paths: BTreeSet<&str> = rest
        .files
        .iter()
        .flat_map(|file| std::iter::once(file.path.as_str()).chain(file.previous_path.as_deref()))
        .collect();
    let git_paths: BTreeSet<&str> = git.iter().flat_map(FileChange::sides).collect();
    // GitHub counts a file once (a rename too), so a complete git list names at least as many
    // distinct paths as `changed_files`; the subset check alone would accept a short one.
    let git_count = u64::try_from(git_paths.len()).unwrap_or(u64::MAX);
    if truncated && git_count < rest.changed_files {
        return Err(FileListError::GitShort {
            git_paths: git_paths.len(),
            changed_files: rest.changed_files,
        });
    }
    let only_in_rest: Vec<&str> = rest_paths.difference(&git_paths).copied().collect();
    let only_in_git: Vec<&str> = if truncated {
        Vec::new()
    } else {
        git_paths.difference(&rest_paths).copied().collect()
    };
    let first = only_in_git.iter().chain(&only_in_rest).min();
    match first {
        None => Ok(()),
        Some(first) => Err(FileListError::Mismatch {
            only_in_git: only_in_git.len(),
            only_in_rest: only_in_rest.len(),
            first: (*first).to_owned(),
        }),
    }
}
