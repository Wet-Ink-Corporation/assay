//! Where a developer's checkout points, for `assay status` (#273): the URL of its `origin`
//! remote, which names the GitHub repository, and the branch `HEAD` is on, which names the PR
//! without `--pr`.
//!
//! Both are read-only git processes through the hardened constructor, reading the repository's
//! own config and refs only (no system or user config: `GIT_CONFIG_NOSYSTEM` and
//! `GIT_CONFIG_GLOBAL=/dev/null` stay set). Anything but one plain answer is refused
//! (wi-b5affa).

use super::super::GitError;
use super::super::command::GitOperation;
use super::super::repository::call;
use super::WorkingTree;
use super::developer::{one_value, unset_is_none};

/// The setting that names the repository `assay status` reports on.
const ORIGIN_URL: &str = "remote.origin.url";

/// What `symbolic-ref` prints before a local branch's name.
const BRANCH_PREFIX: &[u8] = b"refs/heads/";

impl WorkingTree {
    /// The URL of the `origin` remote, from the repository's own config, or `None` when it has
    /// none.
    ///
    /// # Errors
    /// [`GitError::Malformed`] when the setting has more than one value, an empty one or one
    /// that is not UTF-8, and [`GitError`] when git cannot run or fails otherwise.
    pub fn origin_url(&self) -> Result<Option<String>, GitError> {
        let operation = GitOperation::ConfigValue;
        let lookup = call(operation, &[ORIGIN_URL], self.limits);
        let Some(captured) = unset_is_none(self.run(lookup))? else {
            return Ok(None);
        };
        let value = one_value(&captured.stdout, operation)?;
        let url = str::from_utf8(value).map_err(|_not_utf8| GitError::Malformed {
            operation,
            detail: "remote.origin.url is not UTF-8",
        })?;
        Ok(Some(url.to_owned()))
    }

    /// The local branch `HEAD` is on (unborn or not), or `None` when `HEAD` is detached.
    ///
    /// # Errors
    /// [`GitError::Malformed`] when `HEAD` names something other than a local branch, or a
    /// name that is not UTF-8, and [`GitError`] when git cannot run or fails otherwise.
    pub fn branch(&self) -> Result<Option<String>, GitError> {
        let operation = GitOperation::SymbolicRef;
        // `symbolic-ref --quiet` exits 1, printing nothing, exactly when `HEAD` is detached.
        let Some(captured) = unset_is_none(self.run(call(operation, &["HEAD"], self.limits)))?
        else {
            return Ok(None);
        };
        captured
            .stdout
            .strip_suffix(b"\n")
            .and_then(|line| line.strip_prefix(BRANCH_PREFIX))
            .filter(|name| !name.is_empty())
            .and_then(|name| str::from_utf8(name).ok())
            .map(|name| Some(name.to_owned()))
            .ok_or(GitError::Malformed {
                operation,
                detail: "symbolic-ref did not print one local branch",
            })
    }
}
