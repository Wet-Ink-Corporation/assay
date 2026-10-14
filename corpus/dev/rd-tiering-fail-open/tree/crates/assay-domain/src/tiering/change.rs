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
    /// Files named by `include_str!`, `include_bytes!` or `#[doc = include_str!(..)]` in a `.rs`
    /// file at base or head (the Z2s `syn` scan): never `trivial`.
    pub include_targets: Vec<String>,
    /// The package-set diff, base vs head.
    pub packages: PackageDiff,
    /// How `.claude/temper-baseline.json` changed, when it did.
    pub temper_baseline: BaselineChange,
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
}

impl FileChange {
    /// A change to `path` that was not renamed.
    #[must_use]
    pub fn new(path: impl Into<String>, lines: Lines) -> Self {
        Self {
            path: path.into(),
            renamed_from: None,
            lines,
        }
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
