//! `assay status [--json] --local` (#130, 03 §6b): the local check's result for the working
//! tree at the working directory, as `assay.status/1` with `source: "local"`.
//!
//! The working tree is snapshotted as `assay check --local` snapshots it, in a scratch
//! repository, for its tree fingerprint; the report for that tree is shown, else the last one
//! written (stale), else none (`check: null`). Nothing here is read by CI.

use std::fmt;
use std::time::SystemTime;

use assay_app::{
    CalendarDateError, CommitId, ContractSchemaError, ContractValidator, FetchedAt,
    LocalStatusError, LocalStatusRequest, ParseCommitIdError, StatusReport, StoredLocalReport,
    local_status,
};

use crate::check::store::{self, ReadError};
use crate::check::{
    CheckCliError, Opened, REPORT_DIR, Scratch, ScratchLeft, TreeVariables, open_worktree,
};
use crate::doctor::write_chain;

/// Why no local status could be shown.
#[derive(Debug)]
pub(crate) enum LocalStatusCliError {
    /// The working tree could not be opened or snapshotted.
    Tree(CheckCliError),
    /// The local report could not be looked up.
    Read(ReadError),
    /// The clock reads a time no status can carry.
    Clock(CalendarDateError),
    /// HEAD is not a commit id the app accepts.
    Head(ParseCommitIdError),
    /// The contract schemas did not compile.
    Contracts(ContractSchemaError),
    /// The status was refused.
    Status(LocalStatusError),
}

impl fmt::Display for LocalStatusCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tree(source) => source.fmt(f),
            Self::Read(source) => source.fmt(f),
            Self::Clock(source) => write!(f, "the system clock: {source}"),
            Self::Head(source) => write_chain(f, source),
            Self::Contracts(source) => write_chain(f, source),
            Self::Status(source) => {
                f.write_str("no local status: ")?;
                write_chain(f, source)
            }
        }
    }
}

impl std::error::Error for LocalStatusCliError {}

/// The local status, and the scratch directory if it could not be removed.
pub(crate) struct Shown {
    pub(crate) report: StatusReport,
    pub(crate) scratch_left: Option<ScratchLeft>,
}

/// The local status of the working tree `variables` name.
pub(crate) fn run(variables: TreeVariables) -> Result<Shown, LocalStatusCliError> {
    let Opened { worktree, git, .. } =
        open_worktree(variables).map_err(LocalStatusCliError::Tree)?;
    let head = worktree
        .resolve("HEAD")
        .map_err(|error| LocalStatusCliError::Tree(CheckCliError::Git(error)))?;
    let scratch = Scratch::new().map_err(LocalStatusCliError::Tree)?;
    let (_repo, snapshot) = scratch
        .snapshot(&worktree, &head, git)
        .map_err(LocalStatusCliError::Tree)?;
    let found = store::find(&worktree.git_dir().join(REPORT_DIR), snapshot.tree())
        .map_err(LocalStatusCliError::Read)?;
    let fetched_at =
        FetchedAt::from_system_time(SystemTime::now()).map_err(LocalStatusCliError::Clock)?;
    let local_head: CommitId = head.as_str().parse().map_err(LocalStatusCliError::Head)?;
    let validator = ContractValidator::new().map_err(LocalStatusCliError::Contracts)?;
    let request = LocalStatusRequest {
        tree: snapshot.tree(),
        local_head: Some(&local_head),
        report: found.as_ref().map(|found| StoredLocalReport {
            tree: &found.tree,
            bytes: &found.bytes,
        }),
        fetched_at: &fetched_at,
    };
    let report = local_status(&request, &validator).map_err(LocalStatusCliError::Status)?;
    Ok(Shown {
        report,
        scratch_left: scratch.close(),
    })
}
