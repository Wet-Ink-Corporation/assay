//! [`ObjectRepo`]: the object-only repository PR objects are fetched into and measured in.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::attributes::{
    self, AttributeAllowlist, AttributeError, AttributeReport, POLICED_ATTRIBUTES,
};
use super::command::{GitEnvironment, GitOperation, git};
use super::numstat::{self, DiffOutcome, FileChange};
use super::sha::{CommitSha, HttpsRemote};
use super::tree::{self, TreeEntry, TreeLimits};
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

    /// Reads the blobs of `entries` (files, executables and symlinks from [`Self::list_tree`])
    /// with one `git cat-file --batch`, in order. The listed sizes are summed and checked
    /// against `limits.max_total_bytes` before anything is read, and each blob must come back
    /// at its listed size.
    ///
    /// # Errors
    /// [`GitError::TooManyBytes`] past the limit, [`GitError::NotABlob`] for a submodule, and
    /// [`GitError`] when cat-file cannot run, fails, or prints anything but the listed blobs.
    pub fn read_blobs(
        &self,
        entries: &[&TreeEntry],
        limits: TreeLimits,
    ) -> Result<Vec<Vec<u8>>, GitError> {
        let mut expected = Vec::with_capacity(entries.len());
        let mut total = 0_u64;
        for entry in entries {
            let (Some(size), true) = (entry.size, tree::is_object_id(&entry.object)) else {
                return Err(GitError::NotABlob {
                    path: entry.path.clone(),
                });
            };
            total = total.saturating_add(size);
            expected.push((entry.object.as_str(), size));
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

    /// The hardened command for `operation` with `args` appended.
    fn command(&self, operation: GitOperation, args: &[&str]) -> Command {
        let mut command = git(&self.git_dir, &self.environment, operation);
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
        let command = self.command(operation, args);
        let captured = process::run(command, input, limits, self.environment.killer())
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
