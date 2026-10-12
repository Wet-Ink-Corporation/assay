//! What tiering reads: the changed files and the facts other stages computed about them.

use std::iter;

use crate::vocabulary::closed_enum;

/// Everything one tiering depends on besides the base globs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    /// The files of `git diff --name-status base...head`, with `--numstat --text` line counts.
    pub files: Vec<FileChange>,
    /// Trust keys that differ between the base and head manifests, found by TOML parsing.
    pub manifests: Vec<ManifestTrustChange>,
    /// Files a `.rs` file at base or head reads at compile time, found by the Z2s `syn` scan
    /// (#64): the targets of `include!`, `include_str!`, `include_bytes!` (including
    /// `#[doc = include_str!(..)]`) and `#[path = ..]`. Each is a canonical repo-relative path
    /// (`/`-separated, no `.` or `..` segments); it is never `trivial`, and matching ignores
    /// ASCII case.
    pub include_targets: Vec<String>,
    /// Source roots of the workspace's `custom-build` and `proc-macro` targets at base and at
    /// head: the directory of a custom build script (`[package] build`), a proc-macro crate's
    /// directory and its `[lib] path`. Any changed file at or under a root is exec surface.
    /// Derived from the parsed manifests (`assay_app::exec_roots`) or filled by #65.
    pub exec_roots: Vec<ExecRoot>,
    /// The package-set diff, base vs head.
    pub packages: PackageDiff,
    /// How `.claude/temper-baseline.json` changed, when it did.
    pub temper_baseline: BaselineChange,
}

/// Where build-time code lives (04 §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecRoot {
    /// A canonical repo-relative file or directory; it and everything under it is exec surface.
    Path(String),
    /// The root could not be placed inside the repository (an unparseable root manifest, or a
    /// build path that is absolute, holds a `\` or climbs out): every file is exec surface.
    Everything,
}

impl ExecRoot {
    /// Whether `path` is this root or lies under it, ignoring ASCII case. An empty `Path`
    /// holds every path, as `Everything` does (fail closed).
    pub(super) fn holds(&self, path: &str) -> bool {
        match self {
            Self::Path(root) => is_under(path, root),
            Self::Everything => true,
        }
    }
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The path at head (at base for a deletion).
    pub path: String,
    /// The path at base when the file was renamed or copied: every rule reads both sides.
    pub renamed_from: Option<String>,
    /// The `--numstat --text` line counts.
    pub lines: Lines,
    /// What git records at the path, on either side.
    pub kind: FileKind,
}

/// What git records at a path (the `--raw` mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileKind {
    /// A regular or executable file.
    #[default]
    File,
    /// A symbolic link (mode `120000`), on either side: never docs and never excluded.
    Symlink,
    /// A submodule commit (mode `160000`), on either side: never docs and never excluded.
    Gitlink,
}

impl FileChange {
    /// A change to a regular file at `path` that was not renamed.
    #[must_use]
    pub fn new(path: impl Into<String>, lines: Lines) -> Self {
        Self {
            path: path.into(),
            renamed_from: None,
            lines,
            kind: FileKind::File,
        }
    }

    /// The same change, recorded by git as `kind`.
    #[must_use]
    pub fn with_kind(self, kind: FileKind) -> Self {
        Self { kind, ..self }
    }

    /// The same change, renamed from `old`.
    #[must_use]
    pub fn renamed_from(self, old: impl Into<String>) -> Self {
        Self {
            renamed_from: Some(old.into()),
            ..self
        }
    }

    /// The head path, then the base path of a rename.
    pub fn sides(&self) -> impl Iterator<Item = &str> {
        iter::once(self.path.as_str()).chain(self.renamed_from.as_deref())
    }
}

/// Whether `path` is the canonical repo-relative form git prints with `-z`: non-empty,
/// `/`-separated segments that are neither empty, `.` nor `..`, no leading `"` (a C-quoted
/// path), no `\` and no control character. Anything else may be an alias of a trust path.
pub(super) fn is_canonical(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('"')
        && !path.chars().any(|c| c == '\\' || c.is_control())
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

/// Whether `path` is `root` or lies under it, ignoring ASCII case; an empty root holds every
/// path (fail closed).
fn is_under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return true;
    }
    let prefix_matches = path
        .get(..root.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(root));
    prefix_matches && matches!(path.as_bytes().get(root.len()), None | Some(b'/'))
}

/// One file's `git diff --numstat --text` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    /// Lines added and removed.
    Counted {
        /// Lines added.
        added: u64,
        /// Lines removed.
        removed: u64,
    },
    /// `-`: git reports the file as binary, so its size is unknown and taken as the maximum.
    Binary,
}

/// A manifest whose trust keys differ between base and head (04 §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTrustChange {
    /// The manifest's path.
    pub path: String,
    /// The key that changed.
    pub key: ManifestKey,
}

closed_enum! {
    /// A manifest key whose change is a trust-path change (04 §2.2).
    pub enum ManifestKey {
        /// `[lints]`.
        Lints => "lints",
        /// `[workspace.lints]`.
        WorkspaceLints => "workspace.lints",
        /// `[patch]`.
        Patch => "patch",
        /// `[replace]`.
        Replace => "replace",
        /// `[workspace] members`.
        WorkspaceMembers => "workspace.members",
        /// `[workspace] exclude`.
        WorkspaceExclude => "workspace.exclude",
        /// `rust-version`, in `[package]` or `[workspace.package]`.
        RustVersion => "rust-version",
        /// `edition`, in `[package]` or `[workspace.package]`.
        Edition => "edition",
        /// A target's `test`, `doctest` or `harness`, or `autotests` or `autobenches`.
        TestSwitch => "test-switch",
        /// One side does not parse, so every key is taken as changed.
        Unparseable => "unparseable",
    }
}

/// The package-set diff of `cargo metadata`, base vs head (04 §2.2, computed by #65).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageDiff {
    /// The diff, one entry per changed package; empty when no package changed.
    Computed(Vec<PackageChange>),
    /// The diff could not be computed: taken as exec surface.
    Unavailable,
}

/// One package that differs between the base and head package sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageChange {
    /// Code that runs at build time was added or altered.
    ExecSurface {
        /// The package name.
        package: String,
        /// Why it is exec surface.
        cause: ExecCause,
    },
    /// A new or bumped registry package with no `custom-build` or `proc-macro` target: at
    /// least `lite`, and the `cargo deny` gate.
    Registry {
        /// The package name.
        package: String,
    },
    /// Any other lockfile dependency change: at least `lite`.
    Lockfile {
        /// The package name.
        package: String,
    },
}

closed_enum! {
    /// Why a package change is exec surface (04 §2.2).
    pub enum ExecCause {
        /// A new or bumped package with a `custom-build` target.
        BuildScript => "build-script",
        /// A crate with a `proc-macro` target.
        ProcMacro => "proc-macro",
        /// A `[build-dependencies]` change.
        BuildDependency => "build-dependency",
        /// The package's `source` changed (registry, git or path).
        SourceChanged => "source-changed",
        /// A path dependency was added.
        PathDependency => "path-dependency",
    }
}

/// How `.claude/temper-baseline.json` changed between base and head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaselineChange {
    /// Both sides parse and no count went up: not a trust-path change (04 §2.2).
    LoweredOrUnchanged,
    /// Anything else, including a file that does not parse or was added or removed.
    #[default]
    Other,
}
