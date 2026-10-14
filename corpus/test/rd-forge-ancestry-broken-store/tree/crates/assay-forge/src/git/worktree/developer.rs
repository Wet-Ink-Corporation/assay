//! What local mode reads of the developer's own git setup besides the repository: the global
//! excludes file their `git add` honours, and whether the working tree is sparse (02 §6).
//!
//! Each lookup is a read-only git process (`config --get`, `ls-files`) through the hardened
//! constructor: no hook or fsmonitor runs. Anything it cannot read as one plain answer is a
//! refusal, never a guess (wi-b5affa).

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use super::super::GitError;
use super::super::command::{GitOperation, Scope, UserDirs};
use super::super::repository::{Call, call};
use super::{WorkingTree, not_a_tree};
use crate::process::Captured;

/// How the working tree's sparse state was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sparse {
    /// No sparse-checkout setting or skip-worktree entry.
    No,
    /// One of them: the scratch index, loaded from `HEAD` without those bits, would read every
    /// file outside the cone as deleted.
    Yes(&'static str),
}

impl WorkingTree {
    /// The excludes file the developer's `git add` reads: `core.excludesFile` as their own
    /// system, user and repository config set it, else `$XDG_CONFIG_HOME/git/ignore`, else
    /// `~/.config/git/ignore`; `None` when neither variable is set.
    ///
    /// # Errors
    /// [`GitError::NotAWorkingTree`] when the setting, or the variable it falls back to, is not
    /// an absolute path, and [`GitError`] when git cannot run or fails otherwise (an unreadable
    /// config file included).
    pub(super) fn excludes_file(&self, user: &UserDirs) -> Result<Option<PathBuf>, GitError> {
        let args = ["core.excludesFile"];
        let scope = Scope {
            user: Some(user),
            ..Scope::default()
        };
        let lookup = Call {
            scope,
            ..call(GitOperation::ConfigPath, &args, self.limits)
        };
        let Some(captured) = unset_is_none(self.run(lookup))? else {
            // The error owns the root it names; the working tree keeps its own (as below).
            return default_excludes(user).map_err(|reason| not_a_tree(self.root.clone(), reason));
        };
        let value = one_value(&captured.stdout, GitOperation::ConfigPath)?;
        let path = PathBuf::from(OsString::from_vec(value.to_vec()));
        if path.is_absolute() {
            Ok(Some(path))
        } else {
            Err(not_a_tree(
                self.root.clone(),
                "its core.excludesFile is not an absolute path",
            ))
        }
    }

    /// Whether the working tree is sparse: `core.sparseCheckout` set, or a skip-worktree entry
    /// in the index. An `info/sparse-checkout` file alone is not: `git sparse-checkout disable`
    /// leaves it behind, and git reads it only while the setting is on.
    ///
    /// # Errors
    /// [`GitError`] when git cannot run, fails or prints something this layer cannot read.
    pub(super) fn sparse(&self, user: &UserDirs) -> Result<Sparse, GitError> {
        let args = ["core.sparseCheckout"];
        let lookup = Call {
            scope: Scope {
                user: Some(user),
                ..Scope::default()
            },
            ..call(GitOperation::ConfigBool, &args, self.limits)
        };
        if let Some(captured) = unset_is_none(self.run(lookup))? {
            match one_value(&captured.stdout, GitOperation::ConfigBool)? {
                b"true" => return Ok(Sparse::Yes("core.sparseCheckout is set")),
                b"false" => {}
                _ => {
                    return Err(GitError::Malformed {
                        operation: GitOperation::ConfigBool,
                        detail: "config --bool printed neither true nor false",
                    });
                }
            }
        }
        let listing = self.run(Call {
            scope: Scope {
                work_tree: Some(&self.root),
                ..Scope::default()
            },
            ..call(GitOperation::ListIndex, &[], self.limits)
        })?;
        if has_skip_worktree(&listing.stdout) {
            return Ok(Sparse::Yes("its index has skip-worktree entries"));
        }
        Ok(Sparse::No)
    }
}

/// git's default excludes file: `$XDG_CONFIG_HOME/git/ignore`, else `$HOME/.config/git/ignore`,
/// else none; the variable it comes from must be absolute, or which file git reads is unclear.
fn default_excludes(user: &UserDirs) -> Result<Option<PathBuf>, &'static str> {
    let (base, rest) = match (user.xdg_config_home(), user.home()) {
        (Some(xdg), _) => (Path::new(xdg), "git/ignore"),
        (None, Some(home)) => (Path::new(home), ".config/git/ignore"),
        (None, None) => return Ok(None),
    };
    if base.is_absolute() {
        Ok(Some(base.join(rest)))
    } else {
        Err("its XDG_CONFIG_HOME or HOME is not an absolute path")
    }
}

/// `result`, with git config's "not set" exit (1) read as `None`.
fn unset_is_none(result: Result<Captured, GitError>) -> Result<Option<Captured>, GitError> {
    match result {
        Ok(captured) => Ok(Some(captured)),
        Err(GitError::Failed { status, .. }) if status.code() == Some(1) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The one NUL-terminated, non-empty value `git config --null --get` printed.
fn one_value(stdout: &[u8], operation: GitOperation) -> Result<&[u8], GitError> {
    stdout
        .strip_suffix(b"\0")
        .filter(|value| !value.is_empty() && !value.contains(&0))
        .ok_or(GitError::Malformed {
            operation,
            detail: "config did not print one value",
        })
}

/// Whether `ls-files -t -z` output holds an `S` (skip-worktree) entry.
fn has_skip_worktree(stdout: &[u8]) -> bool {
    stdout
        .split(|&byte| byte == 0)
        .any(|record| record.starts_with(b"S "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_value_is_one_non_empty_nul_terminated_field() {
        assert_eq!(
            one_value(b"/home/dev/ignore\0", GitOperation::ConfigPath).ok(),
            Some(b"/home/dev/ignore".as_slice())
        );
        for refused in [&b""[..], b"\0", b"/a", b"/a\0/b\0"] {
            assert!(
                matches!(
                    one_value(refused, GitOperation::ConfigPath),
                    Err(GitError::Malformed { .. })
                ),
                "{refused:?}"
            );
        }
    }

    fn dirs(home: Option<&str>, xdg: Option<&str>) -> UserDirs {
        UserDirs::new(home.map(OsString::from), xdg.map(OsString::from))
    }

    #[test]
    fn the_default_excludes_file_follows_xdg_then_home() {
        assert_eq!(
            default_excludes(&dirs(Some("/h"), Some("/x"))),
            Ok(Some(PathBuf::from("/x/git/ignore")))
        );
        assert_eq!(
            default_excludes(&dirs(Some("/h"), Some(""))),
            Ok(Some(PathBuf::from("/h/.config/git/ignore"))),
            "an empty XDG_CONFIG_HOME is unset"
        );
        assert_eq!(default_excludes(&dirs(None, None)), Ok(None));
        let relative = Err("its XDG_CONFIG_HOME or HOME is not an absolute path");
        assert_eq!(default_excludes(&dirs(Some("h"), None)), relative);
        assert_eq!(default_excludes(&dirs(Some("/h"), Some("x"))), relative);
    }

    #[test]
    fn only_an_s_tag_is_skip_worktree() {
        assert!(has_skip_worktree(b"H a\0S d/b\0"));
        assert!(!has_skip_worktree(b"H a\0H S b\0"));
        assert!(!has_skip_worktree(b""));
    }
}
