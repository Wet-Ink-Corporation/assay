//! CP1, the local pre-push checkpoint (#108, `docs/architecture/04-pipeline.md` §9): before
//! the snapshot, `assay check --local` lists the open Weigh-In items with
//! `weighin open --repo --json` and reads those of the current branch with
//! [`assay_app::checkpoint`]. A blocking item fails the check (exit 1), a pending-triage one
//! only warns, and output weighin would not write stops the check (exit 2) before anything
//! costly runs. `git push --no-verify` skips the pre-push hook, and with it all of this.
//!
//! The program is `ASSAY_WEIGHIN`, which must be absolute, else the first `weighin` on `PATH`.
//! With neither, or on a detached `HEAD`, CP1 says it did not check and does not block: it is
//! a convenience where a ledger is present (P9), never a trust anchor.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::time::Duration;

use assay_app::{Checkpoint, MAX_OPEN_BYTES, MAX_OPEN_ITEMS, OpenItemsError, checkpoint};
use assay_forge::git::{GitError, WorkingTree};
use assay_forge::process::{self, GroupKiller, Limits, ProcessError};

use crate::doctor::write_chain;

/// The variable naming the `weighin` program.
pub(crate) const PROGRAM_VAR: &str = "ASSAY_WEIGHIN";

/// The variables `weighin` is given, when set: where to find programs, the developer's home
/// and the ledger file.
const PASSED: [&str; 3] = ["PATH", "HOME", "WEIGHIN_STORE"];

/// Wall-clock cap of `weighin open`.
const TIMEOUT: Duration = Duration::from_mins(1);

/// What the user is told about skipping CP1 when it stops the check.
const SKIP_HINT: &str = "git push --no-verify skips this check";

/// What CP1 found.
#[derive(Debug)]
pub(crate) enum WeighInCheck {
    /// The branch's items were read.
    Checked(Checkpoint),
    /// CP1 did not run, for this reason.
    NotChecked(NotChecked),
}

/// Why CP1 did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotChecked {
    /// No `weighin` to run.
    NoProgram,
    /// `HEAD` is on no branch, and items are filtered by branch.
    Detached,
}

impl WeighInCheck {
    /// Whether a blocking item fails the check.
    pub(crate) const fn blocks_push(&self) -> bool {
        match self {
            Self::Checked(checkpoint) => checkpoint.blocks_push(),
            Self::NotChecked(_) => false,
        }
    }

    /// The terminal report's Weigh-In section.
    pub(crate) fn render(&self) -> String {
        match self {
            Self::Checked(checkpoint) => checkpoint.render(),
            Self::NotChecked(NotChecked::NoProgram) => {
                "weigh-in: not checked: no weighin on PATH and ASSAY_WEIGHIN is not set\n"
                    .to_owned()
            }
            Self::NotChecked(NotChecked::Detached) => {
                "weigh-in: not checked: HEAD is detached, and items are filtered by branch\n"
                    .to_owned()
            }
        }
    }
}

/// Why CP1 stopped the check.
#[derive(Debug)]
pub(crate) enum WeighInError {
    /// `ASSAY_WEIGHIN` is not an absolute path.
    RelativeProgram(PathBuf),
    /// git could not say which branch `HEAD` is on.
    Branch(GitError),
    /// `weighin` could not be run to completion within its bounds.
    Run(ProcessError),
    /// `weighin` exited unsuccessfully.
    Exit(ExitStatus),
    /// Its output was refused.
    Refused(OpenItemsError),
}

impl fmt::Display for WeighInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeProgram(path) => write!(
                f,
                "{PROGRAM_VAR} names {}, which is not an absolute path",
                path.display()
            ),
            Self::Branch(source) => {
                f.write_str("cannot read the branch HEAD is on: ")?;
                write_chain(f, source)
            }
            Self::Run(source) => {
                f.write_str("weighin open did not run: ")?;
                write_chain(f, source)?;
                write!(f, " ({SKIP_HINT})")
            }
            Self::Exit(status) => write!(f, "weighin open failed: {status} ({SKIP_HINT})"),
            Self::Refused(source) => {
                write_chain(f, source)?;
                write!(f, " ({SKIP_HINT})")
            }
        }
    }
}

impl std::error::Error for WeighInError {}

/// Runs CP1 for `worktree` with the variables of `vars` and `killer` for the process group.
pub(crate) fn run(
    worktree: &WorkingTree,
    vars: &[(OsString, OsString)],
    killer: &GroupKiller,
) -> Result<WeighInCheck, WeighInError> {
    let Some(program) = program(vars)? else {
        return Ok(WeighInCheck::NotChecked(NotChecked::NoProgram));
    };
    let Some(branch) = worktree.branch().map_err(WeighInError::Branch)? else {
        return Ok(WeighInCheck::NotChecked(NotChecked::Detached));
    };
    // One more than is read, so a list `--top` cut short is seen and refused.
    let top = MAX_OPEN_ITEMS.saturating_add(1).to_string();
    let mut command = Command::new(program);
    command
        .args(["open", "--repo", "--json", "--top", &top])
        .current_dir(worktree.root())
        .env_clear()
        .envs(
            vars.iter()
                .filter(|(name, _)| PASSED.iter().any(|kept| name == kept))
                .map(|(name, value)| (name, value)),
        );
    let limits = Limits {
        timeout: TIMEOUT,
        max_output_bytes: MAX_OPEN_BYTES,
    };
    let captured = process::run(command, &[], limits, killer).map_err(WeighInError::Run)?;
    if !captured.status.success() {
        return Err(WeighInError::Exit(captured.status));
    }
    checkpoint(&captured.stdout, &branch)
        .map(WeighInCheck::Checked)
        .map_err(WeighInError::Refused)
}

/// The `weighin` to run: `ASSAY_WEIGHIN` when set and not empty, which must be absolute, else
/// the first `weighin` on `PATH`, else none.
fn program(vars: &[(OsString, OsString)]) -> Result<Option<PathBuf>, WeighInError> {
    let var = |wanted: &str| {
        vars.iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.as_os_str())
            .filter(|value| !value.is_empty())
    };
    if let Some(given) = var(PROGRAM_VAR) {
        let given = PathBuf::from(given);
        return if given.is_absolute() {
            Ok(Some(given))
        } else {
            Err(WeighInError::RelativeProgram(given))
        };
    }
    Ok(var("PATH")
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("weighin"))
        .find(|candidate| candidate.is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    #[test]
    fn an_empty_program_variable_falls_back_to_path() {
        let found = program(&vars(&[
            (PROGRAM_VAR, ""),
            ("PATH", "relative:/nonexistent"),
        ]));

        assert!(matches!(found, Ok(None)), "{found:?}");
    }

    #[test]
    fn not_checked_never_blocks() {
        for reason in [NotChecked::NoProgram, NotChecked::Detached] {
            assert!(!WeighInCheck::NotChecked(reason).blocks_push());
        }
    }
}
