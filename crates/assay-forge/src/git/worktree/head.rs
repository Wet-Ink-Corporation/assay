//! The branch a developer's `HEAD` is on, read without local mode's snapshot preconditions:
//! `assay pre-push` (#108, 04 §9 CP1) needs only that, and must not refuse a sparse tree or
//! an unusual excludes setting that would never reach a snapshot.

use std::path::{Path, PathBuf};

use super::{absolute_lines, dot_git, not_a_tree};
use crate::git::GitError;
use crate::git::command::{GitEnvironment, GitOperation};
use crate::git::repository::{GitLimits, call, run_in};

/// A developer's repository, opened only to read the branch `HEAD` is on. Nothing here writes
/// to it, lists its index or reads its excludes.
#[derive(Debug)]
pub struct RepositoryHead {
    root: PathBuf,
    git_dir: PathBuf,
    environment: GitEnvironment,
    limits: GitLimits,
}

impl RepositoryHead {
    /// The repository whose working tree is `root`, found as [`super::WorkingTree::open`] finds
    /// it (`root/.git`, or a `gitdir:` file), but with none of its checks on the index or the
    /// excludes: a sparse or skip-worktree tree opens.
    ///
    /// # Errors
    /// [`GitError::NotAWorkingTree`] for a relative `root`, a `root` without `.git`, a `.git`
    /// that is a symlink or a file other than one `gitdir:` line of at most 4096 bytes, or a git
    /// directory git does not accept; [`GitError`] when git cannot run or prints something this
    /// layer cannot read.
    pub fn open(
        root: impl Into<PathBuf>,
        environment: GitEnvironment,
        limits: GitLimits,
    ) -> Result<Self, GitError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(not_a_tree(root, "the path is not absolute"));
        }
        let named = match dot_git(&root) {
            Ok(named) => named,
            Err(reason) => return Err(not_a_tree(root, reason)),
        };
        let args = ["--absolute-git-dir"];
        let captured = match run_in(
            &named,
            &environment,
            limits,
            call(GitOperation::RevParse, &args, limits),
        ) {
            Err(GitError::Failed { .. }) => {
                return Err(not_a_tree(root, "git does not read it as a repository"));
            }
            other => other?,
        };
        let [git_dir] = absolute_lines(&captured.stdout)?;
        Ok(Self {
            root,
            git_dir,
            environment,
            limits,
        })
    }

    /// The working tree's root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The branch `HEAD` is on, as [`super::WorkingTree::branch`] reads it.
    ///
    /// # Errors
    /// As [`super::WorkingTree::branch`].
    pub fn branch(&self) -> Result<Option<String>, GitError> {
        branch_in(&self.git_dir, &self.environment, self.limits)
    }
}

/// The branch `HEAD` of the repository at `git_dir` is on, by its name under `refs/heads/`,
/// or `None` when it is detached.
pub(super) fn branch_in(
    git_dir: &Path,
    environment: &GitEnvironment,
    limits: GitLimits,
) -> Result<Option<String>, GitError> {
    // `HEAD` is a constant, so needs no `--end-of-options`, which rev-parse would echo.
    let args = ["--symbolic-full-name", "HEAD"];
    let captured = run_in(
        git_dir,
        environment,
        limits,
        call(GitOperation::RevParse, &args, limits),
    )?;
    let malformed = GitError::Malformed {
        operation: GitOperation::RevParse,
        detail: "rev-parse did not print one ref HEAD names",
    };
    let Some(name) = std::str::from_utf8(&captured.stdout)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
    else {
        return Err(malformed);
    };
    if name == "HEAD" {
        return Ok(None);
    }
    match name.strip_prefix("refs/heads/") {
        Some(branch) if !branch.is_empty() && !branch.contains('\n') => Ok(Some(branch.to_owned())),
        _ => Err(malformed),
    }
}
