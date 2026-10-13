//! [`ObjectRepo`]: the object-only repository PR objects are fetched into and measured in.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::attributes::{
    self, AttributeAllowlist, AttributeError, AttributeReport, POLICED_ATTRIBUTES,
};
use super::changes::{self, ChangeListing};
use super::command::{GitEnvironment, GitOperation, Scope, git_in};
use super::numstat::{self, DiffOutcome, FileChange};
use super::sha::{CommitSha, HttpsRemote};
use super::tree::{self, BlobRef, TreeEntry, TreeLimits};
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

/// One git invocation: the operation, its arguments, its stdin, its wall-clock limit and where
/// it works besides the repository.
#[derive(Debug, Clone, Copy)]
pub(super) struct Call<'a> {
    pub(super) operation: GitOperation,
    pub(super) args: &'a [&'a str],
    pub(super) input: &'a [u8],
    pub(super) timeout: Duration,
    pub(super) scope: Scope<'a>,
}

/// A bare repository holding PR objects as data, with no worktree, so nothing in it is ever
/// checked out (`docs/architecture/03-contracts.md` §9). Every git process it starts is
/// hardened and bounded.
#[derive(Debug)]
pub struct ObjectRepo {
    git_dir: PathBuf,
    environment: GitEnvironment,
    limits: GitLimits,
}

impl ObjectRepo {
    /// Creates a bare repository at `git_dir`, which must not exist or be empty: an existing
    /// repository could carry config the hardened flags do not override. Every git process
    /// it starts inherits only `environment`.
    ///
    /// # Errors
    /// [`GitError::DirectoryNotEmpty`] when `git_dir` has contents, [`GitError::Inspect`] when
    /// it cannot be read, and [`GitError`] when `git init --bare` cannot run or fails.
    pub fn init(
        git_dir: impl Into<PathBuf>,
        environment: GitEnvironment,
        limits: GitLimits,
    ) -> Result<Self, GitError> {
        let git_dir = git_dir.into();
        match fs::read_dir(&git_dir) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(GitError::DirectoryNotEmpty { git_dir });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(GitError::Inspect { git_dir, source }),
        }
        let repo = Self {
            git_dir,
            environment,
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
        let Some(captured) = self.capped_diff(&args, cap)? else {
            return Ok(DiffOutcome::NeedsSplit);
        };
        numstat::parse(&captured.stdout)
            .map(DiffOutcome::Measured)
            .map_err(|Malformed(detail)| GitError::Malformed {
                operation: GitOperation::Diff,
                detail,
            })
    }

    /// The PR's changed files for `plan`: `git diff --raw` (the name-status with both sides'
    /// modes) and `--numstat --text`, each with rename detection over `base...head`, so the
    /// list runs from the merge base as GitHub's does, never against commits the base gained
    /// after the fork. Both diffs together run within the wall-clock `cap`.
    ///
    /// # Errors
    /// [`GitError`] when a diff cannot run or fails (including when the commits share no
    /// merge base), or when the two diffs print output this layer cannot read or that
    /// disagrees on any file. Running past `cap` is not an error:
    /// it is [`ChangeListing::NeedsSplit`].
    pub fn changed_files(
        &self,
        base: &CommitSha,
        head: &CommitSha,
        cap: Duration,
    ) -> Result<ChangeListing, GitError> {
        // One deadline for both diffs, measured from here so it cannot overflow an `Instant`.
        let started = Instant::now();
        let range = format!("{base}...{head}");
        let raw = ["--raw", "-z", "--no-abbrev", "-M", &range, "--"];
        let Some(raw) = self.capped_diff(&raw, cap)? else {
            return Ok(ChangeListing::NeedsSplit);
        };
        let Some(left) = remaining(cap, started.elapsed()) else {
            return Ok(ChangeListing::NeedsSplit);
        };
        let numstat = ["--numstat", "-z", "--text", "-M", &range, "--"];
        let Some(numstat) = self.capped_diff(&numstat, left)? else {
            return Ok(ChangeListing::NeedsSplit);
        };
        changes::parse(&raw.stdout, &numstat.stdout)
            .map(ChangeListing::Listed)
            .map_err(|Malformed(detail)| GitError::Malformed {
                operation: GitOperation::Diff,
                detail,
            })
    }

    /// Runs `git diff` with `args` within `cap`; `None` when it ran past it.
    fn capped_diff(&self, args: &[&str], cap: Duration) -> Result<Option<Captured>, GitError> {
        match self.run(Call {
            timeout: cap,
            ..self.call(GitOperation::Diff, args)
        }) {
            Err(GitError::Process {
                source: ProcessError::TimedOut,
                ..
            }) => Ok(None),
            ran => ran.map(Some),
        }
    }

    /// Checks the `.gitattributes` of `head` against every path in `changes`, both sides of
    /// each rename: `diff`, `text`, `filter`, `working-tree-encoding`, `eol`, `crlf` and
    /// `ident` must all be `unspecified`, except on paths `allowlist` admits, which the
    /// returned report lists as opaque for downstream gates to treat as fully changed.
    ///
    /// # Errors
    /// [`AttributeError::Forbidden`] listing each policed attribute that is set, unset or
    /// valued on a path the allowlist does not admit; [`AttributeError::Git`] when
    /// `git check-attr` cannot run or fails.
    pub fn police_attributes(
        &self,
        head: &CommitSha,
        changes: &[FileChange],
        allowlist: &AttributeAllowlist,
    ) -> Result<AttributeReport, AttributeError> {
        let mut input = Vec::new();
        let paths = changes
            .iter()
            .flat_map(|change| [Some(change.path()), change.previous_path()])
            .flatten();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(b'\0');
        }
        if input.is_empty() {
            return Ok(AttributeReport::default());
        }
        let mut args = vec!["--source", head.as_str()];
        args.extend(POLICED_ATTRIBUTES);
        let captured = self.run(Call {
            input: &input,
            ..self.call(GitOperation::CheckAttr, &args)
        })?;
        let found = attributes::violations(&captured.stdout).map_err(|Malformed(detail)| {
            GitError::Malformed {
                operation: GitOperation::CheckAttr,
                detail,
            }
        })?;
        attributes::judge(found, allowlist)
    }

    /// Lists the entries at or under `path` in `commit`'s tree (the whole tree for `None`), in
    /// git's path order, with their kinds, object ids and sizes; nothing is read. `path` is
    /// literal (ls-tree matches whole path components and expands no wildcard), so
    /// `.claude/temper` never matches `.claude/temper.json`.
    ///
    /// # Errors
    /// [`GitError::TooManyEntries`] past `limits.max_entries`, and [`GitError`] when `ls-tree`
    /// cannot run, fails, or prints output this layer cannot read (an unknown mode, a short
    /// object id, a non-UTF-8 path).
    pub fn list_tree(
        &self,
        commit: &CommitSha,
        path: Option<&str>,
        limits: TreeLimits,
    ) -> Result<Vec<TreeEntry>, GitError> {
        let mut args = vec![commit.as_str(), "--"];
        args.extend(path);
        let captured = self.run(self.call(GitOperation::ListTree, &args))?;
        let entries =
            tree::parse(&captured.stdout).map_err(|Malformed(detail)| GitError::Malformed {
                operation: GitOperation::ListTree,
                detail,
            })?;
        if entries.len() > limits.max_entries {
            return Err(GitError::TooManyEntries {
                max: limits.max_entries,
                found: entries.len(),
            });
        }
        Ok(entries)
    }

    /// Reads `blobs` (files, executables and symlinks from [`Self::list_tree`], borrowed as
    /// [`BlobRef`]s) with one `git cat-file --batch`, in order. The listed sizes are summed and
    /// checked against `limits.max_total_bytes` before anything is read, and each blob must
    /// come back at its listed size.
    ///
    /// # Errors
    /// [`GitError::TooManyBytes`] past the limit, [`GitError::NotABlob`] for a submodule, and
    /// [`GitError`] when cat-file cannot run, fails, or prints anything but the listed blobs.
    pub fn read_blobs(
        &self,
        blobs: &[BlobRef<'_>],
        limits: TreeLimits,
    ) -> Result<Vec<Vec<u8>>, GitError> {
        let mut expected = Vec::with_capacity(blobs.len());
        let mut total = 0_u64;
        for blob in blobs {
            let (Some(size), true) = (blob.size, tree::is_object_id(blob.object)) else {
                return Err(GitError::NotABlob {
                    // The error owns the path it names.
                    path: blob.path.to_owned(),
                });
            };
            total = total.saturating_add(size);
            expected.push((blob.object, size));
        }
        if total > limits.max_total_bytes {
            return Err(GitError::TooManyBytes {
                max: limits.max_total_bytes,
                found: total,
            });
        }
        if expected.is_empty() {
            return Ok(Vec::new());
        }
        let input: String = expected
            .iter()
            .flat_map(|(object, _)| [*object, "\n"])
            .collect();
        let captured = self.run(Call {
            input: input.as_bytes(),
            ..self.call(GitOperation::ReadBlobs, &[])
        })?;
        tree::parse_batch(&captured.stdout, &expected).map_err(|Malformed(detail)| {
            GitError::Malformed {
                operation: GitOperation::ReadBlobs,
                detail,
            }
        })
    }

    /// The object id `bytes` have as a blob in this repository (`git hash-object --no-filters
    /// --stdin`, nothing written), so content can be compared with a listing without reading
    /// the listed blob.
    ///
    /// # Errors
    /// [`GitError`] when hash-object cannot run, fails or prints something other than an id.
    pub fn hash_blob(&self, bytes: &[u8]) -> Result<String, GitError> {
        let captured = self.run(Call {
            input: bytes,
            ..self.call(GitOperation::HashBlob, &[])
        })?;
        str::from_utf8(&captured.stdout)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .filter(|id| tree::is_object_id(id))
            .map(str::to_owned)
            .ok_or(GitError::Malformed {
                operation: GitOperation::HashBlob,
                detail: "hash-object did not print one object id",
            })
    }

    /// A call of `operation` with `args`, no stdin, no scope and the repository's timeout.
    pub(super) fn call<'a>(&self, operation: GitOperation, args: &'a [&'a str]) -> Call<'a> {
        call(operation, args, self.limits)
    }

    /// Runs `call` in this repository within its timeout and the output cap; a non-zero exit is
    /// an error.
    pub(super) fn run(&self, call: Call<'_>) -> Result<Captured, GitError> {
        run_in(&self.git_dir, &self.environment, self.limits, call)
    }
}

/// A call of `operation` with `args`, no stdin, no scope and `limits`' timeout.
pub(super) const fn call<'a>(
    operation: GitOperation,
    args: &'a [&'a str],
    limits: GitLimits,
) -> Call<'a> {
    Call {
        operation,
        args,
        input: b"",
        timeout: limits.timeout,
        scope: Scope {
            work_tree: None,
            index: None,
            user: None,
        },
    }
}

/// Runs `call` against `git_dir` within its timeout and `limits`' output cap; a non-zero exit
/// is an error.
pub(super) fn run_in(
    git_dir: &Path,
    environment: &GitEnvironment,
    limits: GitLimits,
    call: Call<'_>,
) -> Result<Captured, GitError> {
    let Call {
        operation,
        args,
        input,
        timeout,
        scope,
    } = call;
    let process_limits = Limits {
        timeout,
        max_output_bytes: limits.max_output_bytes,
    };
    let mut command: Command = git_in(git_dir, scope, environment, operation);
    command.args(args);
    let captured = process::run(command, input, process_limits, environment.killer())
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

/// What is left of `cap` once `spent` has elapsed; `None` when nothing is.
fn remaining(cap: Duration, spent: Duration) -> Option<Duration> {
    cap.checked_sub(spent).filter(|left| !left.is_zero())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    impl ObjectRepo {
        /// The hardened command for `operation` with `args` appended, as [`run_in`] builds it.
        fn command(&self, operation: GitOperation, args: &[&str]) -> Command {
            let mut command = git_in(
                &self.git_dir,
                Scope::default(),
                &self.environment,
                operation,
            );
            command.args(args);
            command
        }
    }

    #[test]
    fn the_second_diff_gets_only_what_the_first_left_of_the_cap() {
        let cap = Duration::from_secs(60);

        let left = [
            remaining(cap, Duration::from_secs(45)),
            remaining(cap, cap),
            remaining(cap, Duration::from_secs(90)),
        ];

        assert_eq!(
            left,
            [Some(Duration::from_secs(15)), None, None],
            "the two diffs share one deadline; none left is needs-split"
        );
    }

    #[test]
    fn fetches_one_commit_by_sha_into_a_private_ref() {
        let repo = ObjectRepo {
            git_dir: PathBuf::from("/repo.git"),
            environment: GitEnvironment::from_process_env().expect("the test host has a kill"),
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
