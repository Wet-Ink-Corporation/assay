//! The hardened git layer for PR objects (`docs/architecture/03-contracts.md` §9, §2.1).
//!
//! PR objects are data. They are fetched by immutable SHA ([`CommitSha`]) over https only
//! ([`HttpsRemote`]) into an object-only [`ObjectRepo`] with no worktree. Every git process is
//! built by one constructor that ignores system and global config and attributes, disables
//! hooks, external diff drivers and textconv, fscks received objects and clears the inherited
//! environment; and every process runs under a wall-clock limit and an output cap (H-13).
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use assay_forge::git::{CommitSha, DiffOutcome, GitLimits, HttpsRemote, ObjectRepo};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let repo = ObjectRepo::init("/tmp/pr.git", GitLimits::default())?;
//! let remote: HttpsRemote = "https://github.com/Wet-Ink-Corporation/assay.git".parse()?;
//! let base: CommitSha = "0671db2374a1928aa85a711de7476edd9263a48a".parse()?;
//! let head: CommitSha = "194c6ac1a5b4c1fa0ad6f2b0b6c2a4d1e7f3a9b2".parse()?;
//! repo.fetch(&remote, &base)?;
//! repo.fetch(&remote, &head)?;
//! match repo.numstat(&base, &head, Duration::from_secs(60))? {
//!     DiffOutcome::Measured(changes) => {
//!         repo.police_attributes(&head, changes.iter().map(|change| change.path()))?;
//!     }
//!     DiffOutcome::NeedsSplit => { /* classify the PR as needs-split */ }
//! }
//! # Ok(())
//! # }
//! ```

use std::process::ExitStatus;

mod attributes;
mod command;
mod numstat;
mod repository;
mod sha;

pub use crate::process::ProcessError;
pub use attributes::{AttributeError, AttributeViolation, is_policed_path};
pub use command::GitOperation;
pub use numstat::{DiffOutcome, FileChange, LineCount};
pub use repository::{GitLimits, ObjectRepo};
pub use sha::{CommitSha, HttpsRemote, ParseRemoteError, ParseShaError};

/// Why a git operation produced no result.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// git could not be run to completion: it did not start, ran past its limit (and was
    /// killed) or wrote more than the output cap.
    #[error("git {operation} did not complete")]
    Process {
        /// The operation that was running.
        operation: GitOperation,
        /// What went wrong with the process.
        #[source]
        source: ProcessError,
    },
    /// git ran and exited unsuccessfully.
    #[error("git {operation} failed ({status}): {stderr}")]
    Failed {
        /// The operation that failed.
        operation: GitOperation,
        /// git's exit status.
        status: ExitStatus,
        /// git's stderr, lossily decoded, trimmed and cut to its first 4096 characters.
        stderr: String,
    },
    /// git succeeded but printed output this layer cannot read.
    #[error("git {operation} printed unreadable output: {detail}")]
    Malformed {
        /// The operation whose output was unreadable.
        operation: GitOperation,
        /// What was wrong with it.
        detail: &'static str,
    },
    /// A fetch succeeded but the repository holds no commit with the requested name.
    #[error("the fetch did not provide commit {commit}")]
    MissingCommit {
        /// The commit that was requested.
        commit: CommitSha,
    },
}

/// git printed output this layer cannot read; the detail names what was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Malformed(&'static str);

/// Splits `-z` output into its NUL-terminated fields; `None` when there is no output.
fn nul_terminated_fields(stdout: &[u8]) -> Result<Option<std::str::Split<'_, char>>, Malformed> {
    if stdout.is_empty() {
        return Ok(None);
    }
    let text = str::from_utf8(stdout).map_err(|_utf8| Malformed("a path is not UTF-8"))?;
    let fields = text
        .strip_suffix('\0')
        .ok_or(Malformed("the output does not end with NUL"))?;
    Ok(Some(fields.split('\0')))
}
