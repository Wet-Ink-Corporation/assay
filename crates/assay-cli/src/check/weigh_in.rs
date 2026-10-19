//! CP1, the local pre-push checkpoint (#108, `docs/architecture/04-pipeline.md` §9): before
//! the snapshot, `assay check --local` lists the open Weigh-In items with
//! `weighin open --repo --json` and reads those of the current branch with
//! [`assay_app::checkpoint`]. A blocking item fails the check (exit 1), a pending-triage one
//! only warns, and output weighin would not write stops the check (exit 2) before anything
//! costly runs. `git push --no-verify` skips the pre-push hook, and with it all of this.
//!
//! The program is `ASSAY_WEIGHIN`, which must be absolute, else the first executable `weighin`
//! on `PATH`.
//! With neither, or on a detached `HEAD`, CP1 says it did not check and does not block: it is
//! a convenience where a ledger is present (P9), never a trust anchor.

use std::ffi::OsString;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

use assay_app::{Checkpoint, MAX_OPEN_BYTES, MAX_OPEN_ITEMS, OpenItemsError, checkpoint};
use assay_forge::git::GitError;
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

/// Runs CP1 in the working tree at `root`, whose current branch `branch` reads, with the
/// variables of `vars` and `killer` for the process group.
///
/// `branch` is the caller's: `check --local` asks its `WorkingTree`, while `pre-push` asks a
/// `RepositoryHead`, which has none of local mode's snapshot preconditions.
pub(crate) fn run(
    root: &Path,
    branch: impl FnOnce() -> Result<Option<String>, GitError>,
    vars: &[(OsString, OsString)],
    killer: &GroupKiller,
) -> Result<WeighInCheck, WeighInError> {
    let Some(program) = program(vars)? else {
        return Ok(WeighInCheck::NotChecked(NotChecked::NoProgram));
    };
    let Some(branch) = branch().map_err(WeighInError::Branch)? else {
        return Ok(WeighInCheck::NotChecked(NotChecked::Detached));
    };
    // One more than is read, so a list `--top` cut short is seen and refused.
    let top = MAX_OPEN_ITEMS.saturating_add(1).to_string();
    let mut command = Command::new(program);
    command
        .args(["open", "--repo", "--json", "--top", &top])
        .current_dir(root)
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
/// the first executable `weighin` on `PATH`, else none.
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
        .find(|candidate| is_executable(candidate)))
}

/// Whether `path` is a regular file (symlinks followed) with an execute bit, as a shell's
/// `PATH` lookup takes it: a file without one shadows nothing.
fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
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
    fn a_weighin_on_path_without_an_execute_bit_is_passed_over() {
        let shadow = tempfile::tempdir().expect("a scratch directory");
        let real = tempfile::tempdir().expect("a scratch directory");
        let shadowing = shadow.path().join("weighin");
        std::fs::write(&shadowing, "not a program").expect("write the shadow");
        std::fs::set_permissions(&shadowing, std::fs::Permissions::from_mode(0o644))
            .expect("chmod the shadow");
        let wanted = real.path().join("weighin");
        std::fs::write(&wanted, "#!/bin/sh\n").expect("write the program");
        std::fs::set_permissions(&wanted, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the program");
        let path = std::env::join_paths([shadow.path(), real.path()]).expect("a PATH");
        let path = path.to_str().expect("scratch paths are UTF-8");

        let found = program(&vars(&[("PATH", path)]));

        assert_eq!(found.ok().flatten(), Some(wanted));
    }

    #[test]
    fn not_checked_never_blocks() {
        for reason in [NotChecked::NoProgram, NotChecked::Detached] {
            assert!(!WeighInCheck::NotChecked(reason).blocks_push());
        }
    }
}
