//! [`ObjectRepo`]: the object-only repository PR objects are fetched into and measured in.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::attributes::{self, AttributeError, POLICED_ATTRIBUTES, is_policed_path};
use super::command::{GitOperation, git};
use super::numstat::{self, DiffOutcome};
use super::sha::{CommitSha, HttpsRemote};
use super::{GitError, Malformed};
use crate::process::{self, Captured, Limits, ProcessError};

/// Default wall-clock limit for one git process other than a capped diff.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Default cap on stdout, and separately stderr, of one git process.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 << 20;

/// Most characters of git's stderr kept in [`GitError::Failed`].
const STDERR_EXCERPT_CHARS: usize = 4096;

/// Namespace for the refs that keep fetched commits reachable.
const FETCHED_REF_PREFIX: &str = "refs/assay/fetched/";

/// Bounds applied to every git process the repository starts (H-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitLimits {
    /// Wall-clock time after which a git process is killed. A diff uses the cap passed to
    /// [`ObjectRepo::numstat`] instead.
    pub timeout: Duration,
    /// Most bytes accepted on stdout, and separately on stderr.
    pub max_output_bytes: usize,
}

impl Default for GitLimits {
    /// Five minutes and 64 MiB.
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

/// One git invocation: the operation, its arguments, its stdin and its wall-clock limit.
#[derive(Debug, Clone, Copy)]
struct Call<'a> {
    operation: GitOperation,
    args: &'a [&'a str],
    input: &'a [u8],
    timeout: Duration,
}

/// A bare repository holding PR objects as data, with no worktree, so nothing in it is ever
/// checked out (`docs/architecture/03-contracts.md` §9). Every git process it starts is
/// hardened and bounded.
#[derive(Debug)]
pub struct ObjectRepo {
    git_dir: PathBuf,
    limits: GitLimits,
}

impl ObjectRepo {
    /// Creates (or reopens) a bare repository at `git_dir`.
    ///
    /// # Errors
    /// [`GitError`] when `git init --bare` cannot run or fails.
    pub fn init(git_dir: impl Into<PathBuf>, limits: GitLimits) -> Result<Self, GitError> {
        let repo = Self {
            git_dir: git_dir.into(),
            limits,
        };
        repo.run(repo.call(GitOperation::Init, &[]))?;
        Ok(repo)
    }

    /// The repository directory.
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Fetches `commit` from `remote` by its SHA, never by a ref name, keeps it reachable under
    /// `refs/assay/fetched/<sha>`, and checks that it arrived as a commit.
    ///
    /// # Errors
    /// [`GitError`] when the fetch cannot run, fails (including fsck rejecting an object), or
    /// leaves no commit with that name.
    pub fn fetch(&self, remote: &HttpsRemote, commit: &CommitSha) -> Result<(), GitError> {
        let refspec = format!("{commit}:{FETCHED_REF_PREFIX}{commit}");
        self.run(self.call(GitOperation::Fetch, &[remote.as_str(), &refspec]))?;
        let object = format!("{commit}^{{commit}}");
        match self.run(self.call(GitOperation::VerifyCommit, &[&object])) {
            Err(GitError::Failed { .. }) => Err(GitError::MissingCommit {
                commit: commit.clone(),
            }),
            verified => verified.map(drop),
        }
    }

    /// Measures the diff from `base` to `head` with `--numstat --text` and rename detection,
    /// within the wall-clock `cap`.
    ///
    /// # Errors
    /// [`GitError`] when the diff cannot run, fails or prints unreadable output. Running past
    /// `cap` is not an error: it is [`DiffOutcome::NeedsSplit`].
    pub fn numstat(
        &self,
        base: &CommitSha,
        head: &CommitSha,
        cap: Duration,
    ) -> Result<DiffOutcome, GitError> {
        let args = [
            "--numstat",
            "-z",
            "--text",
            "-M",
            base.as_str(),
            head.as_str(),
            "--",
        ];
        match self.run(Call {
            timeout: cap,
            ..self.call(GitOperation::Diff, &args)
        }) {
            Err(GitError::Process {
                source: ProcessError::TimedOut,
                ..
            }) => Ok(DiffOutcome::NeedsSplit),
            Err(error) => Err(error),
            Ok(captured) => numstat::parse(&captured.stdout)
                .map(DiffOutcome::Measured)
                .map_err(|Malformed(detail)| GitError::Malformed {
                    operation: GitOperation::Diff,
                    detail,
                }),
        }
    }

    /// Checks the `.gitattributes` of `head` against every policed path in `changed` (see
    /// [`is_policed_path`](super::is_policed_path)): `diff`, `text`, `filter` and
    /// `working-tree-encoding` must all be `unspecified`.
    ///
    /// # Errors
    /// [`AttributeError::Forbidden`] listing each policed attribute that is set, unset or
    /// valued; [`AttributeError::Git`] when `git check-attr` cannot run or fails.
    pub fn police_attributes<'a>(
        &self,
        head: &CommitSha,
        changed: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), AttributeError> {
        let mut input = Vec::new();
        for path in changed.into_iter().filter(|path| is_policed_path(path)) {
            input.extend_from_slice(path.as_bytes());
            input.push(b'\0');
        }
        if input.is_empty() {
            return Ok(());
        }
        let mut args = vec!["--source", head.as_str()];
        args.extend(POLICED_ATTRIBUTES);
        let captured = self.run(Call {
            input: &input,
            ..self.call(GitOperation::CheckAttr, &args)
        })?;
        let violations =
            attributes::violations(&captured.stdout).map_err(|Malformed(detail)| {
                GitError::Malformed {
                    operation: GitOperation::CheckAttr,
                    detail,
                }
            })?;
        if violations.is_empty() {
            Ok(())
        } else {
            Err(AttributeError::Forbidden { violations })
        }
    }

    /// The hardened command for `operation` with `args` appended.
    fn command(&self, operation: GitOperation, args: &[&str]) -> Command {
        let mut command = git(&self.git_dir, operation);
        command.args(args);
        command
    }

    /// A call of `operation` with `args`, no stdin and the repository's timeout.
    fn call<'a>(&self, operation: GitOperation, args: &'a [&'a str]) -> Call<'a> {
        Call {
            operation,
            args,
            input: b"",
            timeout: self.limits.timeout,
        }
    }

    /// Runs `call` within its timeout and the output cap; a non-zero exit is an error.
    fn run(&self, call: Call<'_>) -> Result<Captured, GitError> {
        let Call {
            operation,
            args,
            input,
            timeout,
        } = call;
        let limits = Limits {
            timeout,
            max_output_bytes: self.limits.max_output_bytes,
        };
        let captured = process::run(self.command(operation, args), input, limits)
            .map_err(|source| GitError::Process { operation, source })?;
        if captured.status.success() {
            Ok(captured)
        } else {
            Err(GitError::Failed {
                operation,
                status: captured.status,
                stderr: String::from_utf8_lossy(&captured.stderr)
                    .trim()
                    .chars()
                    .take(STDERR_EXCERPT_CHARS)
                    .collect(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn fetches_one_commit_by_sha_into_a_private_ref() {
        let repo = ObjectRepo {
            git_dir: PathBuf::from("/repo.git"),
            limits: GitLimits::default(),
        };
        let sha = "0671db2374a1928aa85a711de7476edd9263a48a";

        let command = repo.command(
            GitOperation::Fetch,
            &[
                "https://github.com/o/r.git",
                &format!("{sha}:{FETCHED_REF_PREFIX}{sha}"),
            ],
        );

        let args: Vec<&OsStr> = command.get_args().collect();
        let fetch = args.iter().position(|arg| *arg == "fetch");
        assert_eq!(
            fetch.and_then(|at| args.get(at..)),
            Some(
                [
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--no-auto-gc",
                    "--no-auto-maintenance",
                    "https://github.com/o/r.git",
                    "0671db2374a1928aa85a711de7476edd9263a48a:refs/assay/fetched/0671db2374a1928aa85a711de7476edd9263a48a",
                ]
                .map(OsStr::new)
                .as_slice()
            ),
            "a fetch names one immutable object and nothing that could move"
        );
    }
}
