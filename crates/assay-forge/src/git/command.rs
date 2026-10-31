//! The single constructor of git processes (`docs/architecture/03-contracts.md` §9).
//!
//! [`git_in`] is the only place in this crate that creates a [`Command`]; every git operation
//! goes through it, so no call path can skip the hardening. A test in `tests/it` fails if
//! another constructor appears.

use std::env;
use std::ffi::{OsStr, OsString};
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
    /// The same fetch from a private https remote, sending its credential header to that URL
    /// only and following no redirect.
    FetchAuthorized,
    /// `git cat-file -e`: check that a fetched commit is present.
    VerifyCommit,
    /// `git diff`, with external diff drivers and textconv disabled.
    Diff,
    /// `git check-attr`, reading paths from stdin.
    CheckAttr,
    /// `git ls-tree -r -z -l`: the entries at or under a path of a commit, with sizes.
    ListTree,
    /// `git cat-file --batch`: the raw bytes of listed blobs, by object id from stdin.
    ReadBlobs,
    /// `git hash-object --no-filters --stdin`: the blob id of some bytes, nothing written.
    HashBlob,
    /// `git merge-base --is-ancestor`: whether one commit is an ancestor of another.
    IsAncestor,
    /// `git range-diff`, with notes, external diff drivers and textconv disabled.
    RangeDiff,
    /// `git rev-list --count`: how many commits a range holds.
    RevListCount,
    /// `git rev-list --parents`: a range's commits with their parents (the eligibility rule).
    RevListParents,
    /// `git merge-tree --write-tree --no-messages`: the tree a conflict-free merge writes.
    MergeTree,
    /// `git rev-parse`: a revision's commit or tree, or a path inside a developer's repository
    /// (local mode, 02 §6).
    RevParse,
    /// `git read-tree`: load a commit's tree into a scratch index.
    ReadTree,
    /// `git add --all`: stage a working tree into a scratch index (local mode).
    AddAll,
    /// `git write-tree`: record a scratch index as a tree.
    WriteTree,
    /// `git commit-tree`: a commit of a recorded tree, with assay's fixed identity and date.
    CommitTree,
    /// `git checkout-index --all`: write a scratch index out, symlinks as plain files.
    CheckoutIndex,
    /// `git config --null --path --get`: one path-valued setting of a developer's repository,
    /// with their system and user config read too (local mode's excludes file).
    ConfigPath,
    /// `git config --null --bool --get`: one boolean setting of a developer's repository.
    ConfigBool,
    /// `git ls-files -t -z`: a developer's index with each entry's tag (`S` is skip-worktree).
    ListIndex,
    /// `git config --null --get-all`: every value of one setting in a developer's repository
    /// config alone (`assay status` reads `remote.origin.url`).
    ConfigValue,
    /// `git push --porcelain` of one ref to an https remote (the ledger's lease refs, 06 §1),
    /// with no redirect followed.
    Push,
    /// The same push to a local bare repository: the only operation allowed the file protocol.
    PushLocal,
}

impl GitOperation {
    /// The subcommand and the flags it always carries.
    const fn leading_args(self) -> &'static [&'static str] {
        match self {
            Self::Init => &["init", "--bare", "--quiet"],
            Self::Fetch | Self::FetchAuthorized => &[
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
            Self::ListTree => &["ls-tree", "-r", "-z", "-l"],
            Self::ReadBlobs => &["cat-file", "--batch"],
            Self::HashBlob => &["hash-object", "--no-filters", "--stdin"],
            Self::IsAncestor => &["merge-base", "--is-ancestor"],
            Self::RangeDiff => &[
                "range-diff",
                "--no-color",
                "--no-notes",
                "--no-ext-diff",
                "--no-textconv",
            ],
            Self::RevListCount => &["rev-list", "--count"],
            Self::RevListParents => &["rev-list", "--parents"],
            Self::MergeTree => &["merge-tree", "--write-tree", "--no-messages"],
            Self::RevParse => &["rev-parse"],
            Self::ReadTree => &["read-tree"],
            Self::AddAll => &["add", "--all"],
            Self::WriteTree => &["write-tree"],
            Self::CommitTree => &["commit-tree"],
            Self::CheckoutIndex => &["checkout-index", "--all"],
            Self::ConfigPath => &["config", "--null", "--path", "--get"],
            Self::ConfigBool => &["config", "--null", "--bool", "--get"],
            Self::ListIndex => &["ls-files", "-t", "-z"],
            Self::ConfigValue => &["config", "--null", "--get-all"],
            Self::Push | Self::PushLocal => &[
                "push",
                "--porcelain",
                "--no-recurse-submodules",
                "--no-signed",
                "--no-follow-tags",
            ],
        }
    }

    /// Whether the operation talks to a remote that may need a credential, and so may carry an
    /// HTTP header: a push, and an authorized fetch.
    const fn carries_header(self) -> bool {
        matches!(self, Self::Push | Self::PushLocal | Self::FetchAuthorized)
    }

    /// Whether the operation may read the developer's system and user config: only a setting's
    /// lookup, and only when its [`Scope`] names the [`UserDirs`].
    const fn reads_user_config(self) -> bool {
        matches!(self, Self::ConfigPath | Self::ConfigBool)
    }

    /// `-c` settings the operation adds to [`HARDENED_CONFIG`]: a checkout writes symlinks as
    /// plain files holding their target, so nothing read from the written tree can follow one
    /// out of it; a push to https and an authorized fetch follow no redirect, so a credential
    /// header goes to its remote's host only; a push to a local bare repository may use the
    /// file protocol.
    const fn extra_config(self) -> &'static [&'static str] {
        match self {
            Self::CheckoutIndex => &["core.symlinks=false"],
            Self::Push | Self::FetchAuthorized => &["http.followRedirects=false"],
            Self::PushLocal => &["protocol.file.allow=always"],
            _ => &[],
        }
    }

    /// Variables the operation adds to [`HARDENED_ENV`]: a snapshot commit has a fixed author,
    /// committer and date, so its id depends only on its tree and parent, and a diff reads every
    /// path it is given as a literal path, so a file named `*` or `:x` names only itself.
    const fn extra_env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::CommitTree => &SNAPSHOT_IDENTITY,
            Self::Diff => &[("GIT_LITERAL_PATHSPECS", "1")],
            _ => &[],
        }
    }
}

/// The identity and date of every snapshot commit (local mode, 02 §6).
const SNAPSHOT_IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "assay"),
    ("GIT_AUTHOR_EMAIL", "assay@localhost"),
    ("GIT_AUTHOR_DATE", "@0 +0000"),
    ("GIT_COMMITTER_NAME", "assay"),
    ("GIT_COMMITTER_EMAIL", "assay@localhost"),
    ("GIT_COMMITTER_DATE", "@0 +0000"),
];

impl fmt::Display for GitOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Init => "init",
            Self::Fetch | Self::FetchAuthorized => "fetch",
            Self::VerifyCommit => "cat-file",
            Self::Diff => "diff",
            Self::CheckAttr => "check-attr",
            Self::ListTree => "ls-tree",
            Self::ReadBlobs => "cat-file --batch",
            Self::HashBlob => "hash-object",
            Self::IsAncestor => "merge-base --is-ancestor",
            Self::RangeDiff => "range-diff",
            Self::RevListCount => "rev-list --count",
            Self::RevListParents => "rev-list --parents",
            Self::MergeTree => "merge-tree",
            Self::RevParse => "rev-parse",
            Self::ReadTree => "read-tree",
            Self::AddAll => "add",
            Self::Push | Self::PushLocal => "push",
            Self::WriteTree => "write-tree",
            Self::CommitTree => "commit-tree",
            Self::CheckoutIndex => "checkout-index",
            Self::ConfigPath | Self::ConfigBool | Self::ConfigValue => "config",
            Self::ListIndex => "ls-files",
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

/// Where a developer's git finds their user configuration: `HOME` and `XDG_CONFIG_HOME`.
///
/// Local mode (02 §6) reads it for one thing, the global excludes file, so files they ignore
/// everywhere (`.env.local`, `.idea/`) stay out of the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserDirs {
    home: Option<OsString>,
    xdg_config_home: Option<OsString>,
}

impl UserDirs {
    /// The developer's `HOME` and `XDG_CONFIG_HOME`, either possibly unset.
    #[must_use]
    pub const fn new(home: Option<OsString>, xdg_config_home: Option<OsString>) -> Self {
        Self {
            home,
            xdg_config_home,
        }
    }

    /// `HOME`.
    pub(super) fn home(&self) -> Option<&OsStr> {
        self.home.as_deref()
    }

    /// `XDG_CONFIG_HOME`, unless unset or empty, which git reads as unset.
    pub(super) fn xdg_config_home(&self) -> Option<&OsStr> {
        self.xdg_config_home
            .as_deref()
            .filter(|value| !value.is_empty())
    }

    fn variables(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        [
            ("HOME", self.home.as_deref()),
            ("XDG_CONFIG_HOME", self.xdg_config_home.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
    }
}

/// Where a git process works besides its repository: a working tree, an index file and the
/// developer's user config, all absent for every operation on PR objects.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Scope<'a> {
    /// `--work-tree`: the tree `add` reads or `checkout-index` writes.
    pub(super) work_tree: Option<&'a Path>,
    /// `GIT_INDEX_FILE`: a scratch index, never a developer's own.
    pub(super) index: Option<&'a Path>,
    /// The developer's user config, read only by a setting's lookup.
    pub(super) user: Option<&'a UserDirs>,
    /// An HTTP header a push or an authorized fetch sends to its remote, and only to it: the
    /// ledger token, or a private repository's.
    pub(super) header: Option<HttpHeader<'a>>,
}

/// An HTTP header for one remote URL, passed as `http.<url>.extraHeader` through
/// `GIT_CONFIG_COUNT` variables, never on the command line, where any process on the host could
/// read it.
#[derive(Clone, Copy)]
pub(super) struct HttpHeader<'a> {
    /// The remote URL the header is sent to.
    pub(super) url: &'a str,
    /// The header line, `Name: value`.
    pub(super) line: &'a str,
}

impl fmt::Debug for HttpHeader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpHeader")
            .field("url", &self.url)
            .field("line", &"<redacted>")
            .finish()
    }
}

/// A hardened `git --git-dir <git_dir> [--work-tree <tree>] -c ... <operation>` command, with
/// `GIT_INDEX_FILE` when `scope` names an index; callers append the operation's remaining
/// arguments.
pub(super) fn git_in(
    git_dir: &Path,
    scope: Scope<'_>,
    environment: &GitEnvironment,
    operation: GitOperation,
) -> Command {
    let mut command = Command::new("git");
    harden_environment(&mut command, environment);
    command.envs(operation.extra_env().iter().copied());
    if let Some(index) = scope.index {
        command.env("GIT_INDEX_FILE", index);
    }
    if let (Some(user), true) = (scope.user, operation.reads_user_config()) {
        // Reading a setting runs no hook or helper; it resolves the value the developer's own
        // git resolves, from the system, user and repository files.
        command.env_remove("GIT_CONFIG_NOSYSTEM");
        command.env_remove("GIT_CONFIG_GLOBAL");
        command.envs(user.variables());
    }
    if let (Some(header), true) = (scope.header, operation.carries_header()) {
        command.env("GIT_CONFIG_COUNT", "1");
        command.env(
            "GIT_CONFIG_KEY_0",
            format!("http.{}.extraHeader", header.url),
        );
        command.env("GIT_CONFIG_VALUE_0", header.line);
    }
    command.arg("--git-dir").arg(git_dir);
    if let Some(work_tree) = scope.work_tree {
        command.arg("--work-tree").arg(work_tree);
    }
    for setting in HARDENED_CONFIG.iter().chain(operation.extra_config()) {
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
    use std::time::Duration;

    use super::*;
    use crate::process::{Limits, run};

    /// Every operation.
    const ALL: [GitOperation; 26] = [
        GitOperation::Init,
        GitOperation::Fetch,
        GitOperation::FetchAuthorized,
        GitOperation::VerifyCommit,
        GitOperation::Diff,
        GitOperation::CheckAttr,
        GitOperation::ListTree,
        GitOperation::ReadBlobs,
        GitOperation::HashBlob,
        GitOperation::IsAncestor,
        GitOperation::RangeDiff,
        GitOperation::RevListCount,
        GitOperation::RevListParents,
        GitOperation::MergeTree,
        GitOperation::RevParse,
        GitOperation::ReadTree,
        GitOperation::AddAll,
        GitOperation::WriteTree,
        GitOperation::CommitTree,
        GitOperation::CheckoutIndex,
        GitOperation::ConfigPath,
        GitOperation::ConfigBool,
        GitOperation::ListIndex,
        GitOperation::ConfigValue,
        GitOperation::Push,
        GitOperation::PushLocal,
    ];

    /// The command for `operation` with no scope, as every operation on PR objects builds it.
    fn git(git_dir: &Path, environment: &GitEnvironment, operation: GitOperation) -> Command {
        git_in(git_dir, Scope::default(), environment, operation)
    }

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
        for operation in ALL {
            let command = hardened(operation);
            let mut expected = vec![
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
            // Only a checkout and the pushes add a setting, after the hardened ones (pinned
            // below).
            expected.extend(operation.extra_config());

            assert_eq!(
                config(&command),
                expected,
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
    fn only_a_checkout_and_the_pushes_add_config() {
        let extras: Vec<(GitOperation, &[&str])> = ALL
            .into_iter()
            .map(|operation| (operation, operation.extra_config()))
            .filter(|(_, extra)| !extra.is_empty())
            .collect();

        assert_eq!(
            extras,
            [
                (
                    GitOperation::FetchAuthorized,
                    ["http.followRedirects=false"].as_slice()
                ),
                (
                    GitOperation::CheckoutIndex,
                    ["core.symlinks=false"].as_slice()
                ),
                (
                    GitOperation::Push,
                    ["http.followRedirects=false"].as_slice()
                ),
                (
                    GitOperation::PushLocal,
                    ["protocol.file.allow=always"].as_slice()
                ),
            ]
        );
    }

    #[test]
    fn a_push_carries_the_section_9_config_and_only_its_own_additions() {
        for (operation, extra) in [
            (GitOperation::Push, "http.followRedirects=false"),
            (GitOperation::PushLocal, "protocol.file.allow=always"),
        ] {
            let command = hardened(operation);

            let mut expected: Vec<&str> = HARDENED_CONFIG.to_vec();
            expected.push(extra);
            assert_eq!(config(&command), expected, "{operation}");
            let pushed: Vec<&OsStr> = args(&command)
                .into_iter()
                .skip_while(|arg| *arg != "push")
                .collect();
            assert_eq!(
                pushed,
                [
                    "push",
                    "--porcelain",
                    "--no-recurse-submodules",
                    "--no-signed",
                    "--no-follow-tags"
                ]
                .map(OsStr::new),
                "{operation}"
            );
        }
        assert!(
            ALL.iter()
                .filter(|operation| **operation != GitOperation::PushLocal)
                .all(|operation| !operation
                    .extra_config()
                    .contains(&"protocol.file.allow=always")),
            "only a local push may use the file protocol"
        );
    }

    #[test]
    fn a_scope_names_its_work_tree_and_index_and_nothing_else() {
        let command = git_in(
            Path::new("/scratch.git"),
            Scope {
                work_tree: Some(Path::new("/home/dev/work")),
                index: Some(Path::new("/scratch.git/snapshot-index")),
                user: None,
                header: None,
            },
            &host(),
            GitOperation::AddAll,
        );

        let envs: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            args(&command).get(..4),
            Some(
                [
                    OsStr::new("--git-dir"),
                    OsStr::new("/scratch.git"),
                    OsStr::new("--work-tree"),
                    OsStr::new("/home/dev/work"),
                ]
                .as_slice()
            )
        );
        assert_eq!(
            envs.get(OsStr::new("GIT_INDEX_FILE")),
            Some(&Some(OsStr::new("/scratch.git/snapshot-index"))),
            "a scratch index, never the developer's"
        );
        assert_eq!(
            hardened(GitOperation::AddAll)
                .get_envs()
                .find(|(name, _)| *name == "GIT_INDEX_FILE"),
            None,
            "no index file without a scope"
        );
    }

    #[test]
    fn a_snapshot_commit_has_a_fixed_identity_and_date() {
        let command = hardened(GitOperation::CommitTree);

        let envs: BTreeMap<_, _> = command.get_envs().collect();
        for (name, value) in [
            ("GIT_AUTHOR_NAME", "assay"),
            ("GIT_AUTHOR_EMAIL", "assay@localhost"),
            ("GIT_AUTHOR_DATE", "@0 +0000"),
            ("GIT_COMMITTER_NAME", "assay"),
            ("GIT_COMMITTER_EMAIL", "assay@localhost"),
            ("GIT_COMMITTER_DATE", "@0 +0000"),
        ] {
            assert_eq!(
                envs.get(OsStr::new(name)),
                Some(&Some(OsStr::new(value))),
                "{name} must be {value}, so the commit id depends only on tree and parent"
            );
        }
    }

    #[test]
    fn every_diff_reads_its_paths_literally() {
        let command = hardened(GitOperation::Diff);

        let envs: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            envs.get(OsStr::new("GIT_LITERAL_PATHSPECS")),
            Some(&Some(OsStr::new("1")))
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

    #[test]
    fn each_operation_carries_its_subcommand_and_fixed_flags() {
        let leading: Vec<(GitOperation, &[&str])> = ALL
            .into_iter()
            .map(|operation| (operation, operation.leading_args()))
            .collect();

        assert_eq!(leading, LEADING);
    }

    /// The flags every fetch carries, authorized or not.
    const FETCH_ARGS: &[&str] = &[
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--no-auto-gc",
        "--no-auto-maintenance",
    ];

    /// The flags every push carries.
    const PUSH_ARGS: &[&str] = &[
        "push",
        "--porcelain",
        "--no-recurse-submodules",
        "--no-signed",
        "--no-follow-tags",
    ];

    /// Each operation's subcommand and fixed flags, in [`ALL`]'s order.
    const LEADING: [(GitOperation, &[&str]); 26] = [
        (GitOperation::Init, &["init", "--bare", "--quiet"]),
        (GitOperation::Fetch, FETCH_ARGS),
        (GitOperation::FetchAuthorized, FETCH_ARGS),
        (GitOperation::VerifyCommit, &["cat-file", "-e"]),
        (
            GitOperation::Diff,
            &["diff", "--no-ext-diff", "--no-textconv", "--no-color"],
        ),
        (GitOperation::CheckAttr, &["check-attr", "-z", "--stdin"]),
        (GitOperation::ListTree, &["ls-tree", "-r", "-z", "-l"]),
        (GitOperation::ReadBlobs, &["cat-file", "--batch"]),
        (
            GitOperation::HashBlob,
            &["hash-object", "--no-filters", "--stdin"],
        ),
        (GitOperation::IsAncestor, &["merge-base", "--is-ancestor"]),
        (
            GitOperation::RangeDiff,
            &[
                "range-diff",
                "--no-color",
                "--no-notes",
                "--no-ext-diff",
                "--no-textconv",
            ],
        ),
        (GitOperation::RevListCount, &["rev-list", "--count"]),
        (GitOperation::RevListParents, &["rev-list", "--parents"]),
        (
            GitOperation::MergeTree,
            &["merge-tree", "--write-tree", "--no-messages"],
        ),
        (GitOperation::RevParse, &["rev-parse"]),
        (GitOperation::ReadTree, &["read-tree"]),
        (GitOperation::AddAll, &["add", "--all"]),
        (GitOperation::WriteTree, &["write-tree"]),
        (GitOperation::CommitTree, &["commit-tree"]),
        (GitOperation::CheckoutIndex, &["checkout-index", "--all"]),
        (
            GitOperation::ConfigPath,
            &["config", "--null", "--path", "--get"],
        ),
        (
            GitOperation::ConfigBool,
            &["config", "--null", "--bool", "--get"],
        ),
        (GitOperation::ListIndex, &["ls-files", "-t", "-z"]),
        (
            GitOperation::ConfigValue,
            &["config", "--null", "--get-all"],
        ),
        (GitOperation::Push, PUSH_ARGS),
        (GitOperation::PushLocal, PUSH_ARGS),
    ];

    #[test]
    fn only_a_setting_lookup_with_user_dirs_reads_the_developer_s_config() {
        let user = UserDirs::new(
            Some(OsString::from("/home/dev")),
            Some(OsString::from("/home/dev/.xdg")),
        );
        let scope = Scope {
            user: Some(&user),
            ..Scope::default()
        };
        let envs_of = |operation, scope| -> BTreeMap<OsString, Option<OsString>> {
            git_in(Path::new("/repo.git"), scope, &host(), operation)
                .get_envs()
                .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
                .collect()
        };

        let lookup = envs_of(GitOperation::ConfigPath, scope);
        let other = envs_of(GitOperation::ListIndex, scope);
        let unscoped = envs_of(GitOperation::ConfigPath, Scope::default());

        for name in ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_NOSYSTEM"] {
            // After `env_clear`, a removed variable is simply absent.
            assert!(
                matches!(lookup.get(OsStr::new(name)), None | Some(None)),
                "{name} is removed for the lookup"
            );
            assert!(
                matches!(other.get(OsStr::new(name)), Some(Some(_))),
                "{name} stays for every other operation"
            );
            assert!(
                matches!(unscoped.get(OsStr::new(name)), Some(Some(_))),
                "{name} stays without user dirs"
            );
        }
        assert_eq!(
            lookup.get(OsStr::new("HOME")),
            Some(&Some(OsString::from("/home/dev")))
        );
        assert_eq!(
            lookup.get(OsStr::new("XDG_CONFIG_HOME")),
            Some(&Some(OsString::from("/home/dev/.xdg")))
        );
        assert_eq!(other.get(OsStr::new("HOME")), None);
    }
}
