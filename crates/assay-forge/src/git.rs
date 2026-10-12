//! The hardened git layer for PR objects (`docs/architecture/03-contracts.md` §9, §2.1).
//!
//! PR objects are data. They are fetched by immutable SHA ([`CommitSha`]) over https only
//! ([`HttpsRemote`]) into an object-only [`ObjectRepo`] with no worktree. Every git process is
//! built by one constructor that ignores system and global config and attributes, disables
//! hooks, external diff drivers and textconv, fscks received objects and clears the inherited
//! environment, reading no in-tree attributes; and every process runs in its own process group
//! under a wall-clock limit and an output cap (H-13).
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use assay_forge::git::{
//!     AttributeAllowlist, CommitSha, DiffOutcome, GitEnvironment, GitLimits, HttpsRemote,
//!     ObjectRepo,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let environment = GitEnvironment::from_process_env()?;
//! let repo = ObjectRepo::init("/tmp/pr.git", environment, GitLimits::default())?;
//! let remote: HttpsRemote = "https://github.com/Wet-Ink-Corporation/assay.git".parse()?;
//! let base: CommitSha = "0671db2374a1928aa85a711de7476edd9263a48a".parse()?;
//! let head: CommitSha = "194c6ac1a5b4c1fa0ad6f2b0b6c2a4d1e7f3a9b2".parse()?;
//! // From the base `.assay/plugins.toml`; empty when it names none.
//! let allowlist = AttributeAllowlist::new(["assets/**"])?;
//! repo.fetch(&remote, &base)?;
//! repo.fetch(&remote, &head)?;
//! match repo.numstat(&base, &head, Duration::from_secs(60))? {
//!     DiffOutcome::Measured(changes) => {
//!         let report = repo.police_attributes(&head, &changes, &allowlist)?;
//!         // Downstream gates treat these as fully changed.
//!         let fully_changed: Vec<&str> = report.opaque_paths().collect();
//!         assert!(fully_changed.iter().all(|path| path.starts_with("assets/")));
//!     }
//!     DiffOutcome::NeedsSplit => { /* classify the PR as needs-split */ }
//! }
//! # Ok(())
//! # }
//! ```

use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

mod attributes;
mod command;
mod glob;
mod numstat;
mod repository;
mod sha;
mod tree;

use crate::process::ProcessError;
pub use attributes::{
    AttributeAllowlist, AttributeError, AttributeReport, PolicedAttribute, is_protected_path,
};
pub use command::{GitEnvironment, GitOperation};
pub use glob::GlobError;
pub use numstat::{DiffOutcome, FileChange, LineCount};
pub use repository::{GitLimits, ObjectRepo};
pub use sha::{CommitSha, HttpsRemote, ParseRemoteError, ParseShaError};
pub use tree::{BlobRef, EntryKind, TreeEntry, TreeLimits};

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
    /// The directory for a new repository already has contents, which could include config
    /// that the hardened flags do not override.
    #[error("{} is not empty; the object repository must start fresh", git_dir.display())]
    DirectoryNotEmpty {
        /// The directory that was refused.
        git_dir: PathBuf,
    },
    /// The directory for a new repository could not be inspected.
    #[error("could not inspect {}", git_dir.display())]
    Inspect {
        /// The directory that was being inspected.
        git_dir: PathBuf,
        /// Why it could not be read.
        #[source]
        source: io::Error,
    },
    /// A tree listing holds more entries than its limit allows; nothing was read.
    #[error("the tree listing has {found} entries, over the limit of {max}")]
    TooManyEntries {
        /// The limit.
        max: usize,
        /// Entries listed.
        found: usize,
    },
    /// The listed sizes of the blobs asked for add up past the limit; nothing was read.
    #[error("the blobs asked for total {found} bytes, over the limit of {max}")]
    TooManyBytes {
        /// The limit.
        max: u64,
        /// The listed total (saturating).
        found: u64,
    },
    /// A blob was asked for an entry that has none (a submodule).
    #[error("{path} is a submodule, which has no blob")]
    NotABlob {
        /// The entry's path.
        path: String,
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
