//! The single constructor of git processes (`docs/architecture/03-contracts.md` §9).
//!
//! [`git`] is the only place in this crate that creates a [`Command`]; every git operation
//! goes through it, so no call path can skip the hardening. A test in `tests/it` fails if
//! another constructor appears.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::process::{GroupKiller, KillerError};

/// Variables set on every git process. Global and system config and system attributes are
/// ignored, credential prompts cannot block, and replace refs cannot substitute objects.
const HARDENED_ENV: [(&str, &str); 6] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_ATTR_NOSYSTEM", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("LC_ALL", "C"),
];

/// The only variables [`GitEnvironment::from_process_env`] takes from assay's environment:
/// `PATH` to find git, and the proxy settings an https fetch may need.
const PASSED_THROUGH: [&str; 5] = ["PATH", "HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy"];

/// `-c` settings on every git process: no hooks, https as the only transport, fsck on every
/// object received, no submodule recursion, a bounded rename search and no fsmonitor daemon.
/// The low-speed abort bounds a stalled https transfer. `attr.tree` points at the empty tree,
/// so no in-tree `.gitattributes` (a bare repository otherwise reads HEAD's) changes what a
/// diff shows; `check-attr --source` still reads the PR's attributes in order to police them.
const HARDENED_CONFIG: [&str; 11] = [
    "core.hooksPath=/dev/null",
    "protocol.allow=never",
    "protocol.https.allow=always",
    "transfer.fsckObjects=true",
    "fetch.recurseSubmodules=false",
    "fetch.fsckObjects=true",
    "diff.renameLimit=1000",
    "core.fsmonitor=false",
    "http.lowSpeedLimit=1000",
    "http.lowSpeedTime=30",
    "attr.tree=4b825dc642cb6eb9a060e54bf8d69288fbee4904",
];

/// The git operations assay performs on PR objects. Each fixes its subcommand and the flags
/// that must accompany it, so for example a diff cannot be built without `--no-ext-diff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitOperation {
    /// `git init --bare`: create the object-only repository.
    Init,
    /// `git fetch` of one commit by SHA.
    Fetch,
    /// `git cat-file -e`: check that a fetched commit is present.
    VerifyCommit,
    /// `git diff`, with external diff drivers and textconv disabled.
    Diff,
    /// `git check-attr`, reading paths from stdin.
    CheckAttr,
    /// `git ls-tree -r -z`: the files at or under a path of a commit.
    ListTree,
    /// `git cat-file blob`: the bytes of one blob, by object name.
    ReadBlob,
}

impl GitOperation {
    /// The subcommand and the flags it always carries.
    const fn leading_args(self) -> &'static [&'static str] {
        match self {
            Self::Init => &["init", "--bare", "--quiet"],
            Self::Fetch => &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--no-auto-gc",
                "--no-auto-maintenance",
            ],
            Self::VerifyCommit => &["cat-file", "-e"],
            Self::Diff => &["diff", "--no-ext-diff", "--no-textconv", "--no-color"],
            Self::CheckAttr => &["check-attr", "-z", "--stdin"],
            Self::ListTree => &["ls-tree", "-r", "-z"],
            Self::ReadBlob => &["cat-file", "blob"],
        }
    }
}

impl fmt::Display for GitOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Init => "init",
            Self::Fetch => "fetch",
            Self::VerifyCommit => "cat-file",
            Self::Diff => "diff",
            Self::CheckAttr => "check-attr",
            Self::ListTree => "ls-tree",
            Self::ReadBlob => "cat-file blob",
        };
        f.write_str(name)
    }
}

/// What git processes take from the host: at most `PATH` and the proxy settings, plus the
/// [`GroupKiller`] that stops a timed-out process group.
///
/// Captured once, at the composition root, and handed to
/// [`ObjectRepo::init`](super::ObjectRepo::init) (ARC-11); every other variable is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEnvironment {
    inherited: Vec<(&'static str, OsString)>,
    killer: GroupKiller,
}

impl GitEnvironment {
    /// Reads `PATH`, `HTTPS_PROXY`, `https_proxy`, `NO_PROXY` and `no_proxy` from this
    /// process's environment (the rest, including `HOME`, `GIT_DIR`, `GIT_CONFIG_*`,
    /// `GIT_EXTERNAL_DIFF` and `GIT_SSH_COMMAND`, is never passed on), checks `/proc` and
    /// finds the system `kill` ([`GroupKiller::system`]).
    ///
    /// # Errors
    /// [`KillerError::ProcUnavailable`] when `/proc` does not describe this process, and
    /// [`KillerError::NotFound`] when the host has no usable `kill`: without either a
    /// timed-out process group could not be stopped, so nothing runs.
    pub fn from_process_env() -> Result<Self, KillerError> {
        let killer = GroupKiller::system()?;
        let inherited = PASSED_THROUGH
            .into_iter()
            .filter_map(|name| env::var_os(name).map(|value| (name, value)))
            .collect();
        Ok(Self { inherited, killer })
    }

    /// Only `PATH`, set to `path` (no proxy), with `killer`. `/proc` was checked when `killer`
    /// was built ([`GroupKiller::system`] and [`GroupKiller::at`] both run the preflight).
    #[must_use]
    pub fn new(path: impl Into<OsString>, killer: GroupKiller) -> Self {
        Self {
            inherited: vec![("PATH", path.into())],
            killer,
        }
    }

    /// The program that kills a timed-out process group.
    pub(super) const fn killer(&self) -> &GroupKiller {
        &self.killer
    }
}

/// A hardened `git --git-dir <git_dir> -c ... <operation>` command; callers append the
/// operation's remaining arguments.
pub(super) fn git(
    git_dir: &Path,
    environment: &GitEnvironment,
    operation: GitOperation,
) -> Command {
    let mut command = Command::new("git");
    harden_environment(&mut command, environment);
    command.arg("--git-dir").arg(git_dir);
    for setting in HARDENED_CONFIG {
        command.arg("-c").arg(setting);
    }
    command.args(operation.leading_args());
    command
}

/// Clears the inherited environment down to `environment` and sets [`HARDENED_ENV`].
fn harden_environment(command: &mut Command, environment: &GitEnvironment) {
    command.env_clear();
    command.envs(
        environment
            .inherited
            .iter()
            .map(|(name, value)| (*name, value)),
    );
    command.envs(HARDENED_ENV);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::time::Duration;

    use super::*;
    use crate::process::{Limits, run};

    fn args(command: &Command) -> Vec<&OsStr> {
        command.get_args().collect()
    }

    /// The `-c` values passed before the subcommand.
    fn config(command: &Command) -> Vec<&OsStr> {
        args(command)
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1])
            .collect()
    }

    fn host() -> GitEnvironment {
        GitEnvironment::from_process_env().expect("the test host has a system kill")
    }

    fn hardened(operation: GitOperation) -> Command {
        git(Path::new("/repo.git"), &host(), operation)
    }

    #[test]
    fn every_command_carries_the_section_9_config() {
        for operation in [
            GitOperation::Init,
            GitOperation::Fetch,
            GitOperation::VerifyCommit,
            GitOperation::Diff,
            GitOperation::CheckAttr,
            GitOperation::ListTree,
            GitOperation::ReadBlob,
        ] {
            let command = hardened(operation);

            assert_eq!(
                config(&command),
                [
                    "core.hooksPath=/dev/null",
                    "protocol.allow=never",
                    "protocol.https.allow=always",
                    "transfer.fsckObjects=true",
                    "fetch.recurseSubmodules=false",
                    "fetch.fsckObjects=true",
                    "diff.renameLimit=1000",
                    "core.fsmonitor=false",
                    "http.lowSpeedLimit=1000",
                    "http.lowSpeedTime=30",
                    "attr.tree=4b825dc642cb6eb9a060e54bf8d69288fbee4904",
                ],
                "{operation} must run with the hardened config"
            );
            assert_eq!(
                args(&command).get(..2),
                Some([OsStr::new("--git-dir"), OsStr::new("/repo.git")].as_slice()),
                "{operation} must name its repository explicitly, never discover one"
            );
        }
    }

    #[test]
    fn every_command_ignores_system_and_global_config_and_attributes() {
        let command = hardened(GitOperation::Fetch);
        let envs: BTreeMap<_, _> = command.get_envs().collect();

        for (name, value) in [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_ATTR_NOSYSTEM", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ] {
            assert_eq!(
                envs.get(OsStr::new(name)),
                Some(&Some(OsStr::new(value))),
                "{name} must be {value}"
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns `env`; Miri cannot spawn processes")]
    fn the_environment_is_cleared_to_an_allowlist() {
        let host = host();
        let mut env = Command::new("env");
        harden_environment(&mut env, &host);
        let limits = Limits {
            timeout: Duration::from_secs(30),
            max_output_bytes: 1 << 16,
        };

        let output = run(env, b"", limits, host.killer()).expect("env runs");

        let stdout = String::from_utf8(output.stdout).expect("env prints UTF-8 here");
        let names: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect();
        let allowed: Vec<&str> = PASSED_THROUGH
            .into_iter()
            .chain(HARDENED_ENV.map(|(name, _)| name))
            .collect();
        assert!(
            names.iter().all(|name| allowed.contains(name)),
            "only allowlisted variables may reach git, got {names:?}"
        );
    }

    #[test]
    fn an_explicit_environment_passes_through_only_what_it_was_given() {
        let killer = GroupKiller::system().expect("the test host has a system kill");
        let environment = GitEnvironment::new("/opt/git/bin", killer);

        let command = git(Path::new("/repo.git"), &environment, GitOperation::Diff);

        let envs: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            envs.get(OsStr::new("PATH")),
            Some(&Some(OsStr::new("/opt/git/bin"))),
            "the configured PATH reaches git"
        );
        assert_eq!(
            envs.get(OsStr::new("HTTPS_PROXY")),
            None,
            "nothing unconfigured is inherited"
        );
    }

    #[test]
    fn every_diff_disables_external_diff_drivers_and_textconv() {
        let command = hardened(GitOperation::Diff);

        let args = args(&command);
        let subcommand = args.iter().position(|arg| *arg == "diff");
        assert_eq!(
            subcommand.and_then(|at| args.get(at..at + 3)),
            Some(
                [
                    OsStr::new("diff"),
                    OsStr::new("--no-ext-diff"),
                    OsStr::new("--no-textconv")
                ]
                .as_slice()
            ),
            "a diff without these flags runs PR-controlled drivers"
        );
    }
}
