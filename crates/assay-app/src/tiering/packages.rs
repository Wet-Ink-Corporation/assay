//! The package sets of base and head, from `cargo metadata` (04 §2.2, #65): the package-set
//! diff and the path crates that run at build time, which feed the domain's
//! `ChangeSet::packages` and `ChangeSet::exec_roots`.

mod diff;
mod metadata;
mod roots;

use assay_domain::{ExecRoot, PackageDiff};

pub use metadata::MetadataError;

/// One commit's package set: every package `cargo metadata --format-version 1 --all-features`
/// lists, run by the pinned base cargo on a sanitised manifest-only copy, with the resolve
/// graph between them.
///
/// [`PackageSet::default`] is the set of a tree with no manifest at all; a tree that has
/// manifests but whose metadata cannot be computed has no set (see [`exec_surface`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSet {
    packages: Vec<Package>,
    /// For each package, by index, the packages it depends on outside development; `None`
    /// when the metadata has no resolve graph.
    graph: Option<Vec<Vec<Edge>>>,
    /// The tree's symlinks, with their resolved targets.
    symlinks: Vec<Symlink>,
}

impl Default for PackageSet {
    /// No package, a resolve graph with nothing in it, and no symlink.
    fn default() -> Self {
        Self {
            packages: Vec::new(),
            graph: Some(Vec::new()),
            symlinks: Vec::new(),
        }
    }
}

/// A symlink of a commit's tree and where it leads: what the manifest copy accepted and
/// wrote as an empty file. Both are repo-relative, `/`-separated paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symlink {
    /// The symlink's own path.
    pub path: String,
    /// Its target, resolved from the symlink's directory.
    pub target: String,
}

/// One package, reduced to what exec-surface detection compares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Package {
    name: String,
    version: String,
    source: Source,
    /// It has a `custom-build` target.
    build_script: bool,
    /// It has a `proc-macro` target.
    proc_macro: bool,
    /// The source files of its library and proc-macro targets (`src_path`), which may lie
    /// outside its directory (`[lib] path`).
    libraries: Vec<Location>,
    /// Its `[build-dependencies]` declarations, sorted.
    build_dependencies: Vec<Declared>,
    /// Its declarations of every kind that name a `path`, sorted.
    path_dependencies: Vec<PathDeclared>,
}

/// Where a package comes from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    /// A path package (`source` is null), at its manifest's directory.
    Path(Location),
    /// cargo's `source` string: a registry (`registry+`, `sparse+`), git or other source.
    Other(String),
}

/// crates.io's index, in the forms cargo records it: the only registry 04 §2.2's reduction
/// trusts (Weigh-In default, #65 review F6); any other registry is a source change.
const CRATES_IO: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];

impl Source {
    /// Whether the source is crates.io, which serves a package only at the checksum the
    /// lockfile records.
    fn is_registry(&self) -> bool {
        matches!(self, Self::Other(source) if CRATES_IO.contains(&source.as_str()))
    }
}

/// A directory, relative to the copy's root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Location {
    /// Inside the copy; `""` is its root.
    Inside(String),
    /// Outside the copy, or not a canonical path under it.
    Outside,
}

/// A dependency declaration, every field cargo reports that changes what is built.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Declared {
    name: String,
    rename: Option<String>,
    req: String,
    source: Option<String>,
    path: Option<Location>,
    target: Option<String>,
    optional: bool,
    default_features: bool,
    features: Vec<String>,
}

/// A declaration that names a `path`: what makes it a path-dependency addition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PathDeclared {
    kind: Option<String>,
    target: Option<String>,
    /// The name it is declared under (its `rename`, else the package name).
    name: String,
    path: Location,
}

/// A resolved dependency that is compiled outside development: a normal or build
/// dependency, or one of a kind cargo did not name before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edge {
    /// The index of the package depended on.
    to: usize,
    /// It is a build dependency (or of a kind cargo did not name before).
    build: bool,
}

/// What a PR changes in the package set and where build-time code lives (04 §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSurface {
    /// For `ChangeSet::packages`: the package-set diff, base vs head.
    pub packages: PackageDiff,
    /// For `ChangeSet::exec_roots`: the declared roots, then the directories (and
    /// out-of-crate library sources) of path crates that run at build time, at base or at
    /// head, sorted; or only [`ExecRoot::Everything`].
    pub exec_roots: Vec<ExecRoot>,
}

/// The package-set diff and every exec root between `base` and `head`: `declared` (the
/// manifests' roots, `manifest_exec_roots` over every manifest) and the build-time path
/// crates.
///
/// `None` is a side whose metadata could not be computed (cargo failed, or its output did not
/// parse): the diff is then [`PackageDiff::Unavailable`] and the only root
/// [`ExecRoot::Everything`] (fail closed). So is any root that holds a symlink of either side
/// whose target it does not hold (#65 review round 3, wi-b5affa): an edit to the target runs
/// at build time, and following targets further is not attempted.
///
/// The diff has one entry per cause, packages in head order and removed packages last:
/// - a package new or bumped with a `custom-build` target is `BuildScript` exec surface, and
///   with a `proc-macro` target `ProcMacro`, including a path package whose version changed;
///   an unchanged package that gains either target is too;
/// - a package whose name is in base only from another source, and a new package from
///   neither crates.io nor a path (git, another registry), is `SourceChanged`;
/// - a package whose `[build-dependencies]` declarations differ from its base counterpart's
///   (the same package, else the same name and source) is `BuildDependency`;
/// - a package that declares a `path` dependency its counterpart did not is `PathDependency`;
/// - any other new or bumped crates.io package is a `Registry` change, and any other new,
///   bumped or removed package a `Lockfile` change.
///
/// The roots are the path crates reached from a build-dependency edge or from a proc-macro
/// crate (itself included), transitively over normal and build edges, on either side: each
/// directory a `Path`, and the copy's root, a directory outside it or a side without a resolve
/// graph [`ExecRoot::Everything`].
///
/// # Examples
///
/// ```
/// use assay_app::{PackageSet, exec_surface};
/// use assay_domain::{ExecRoot, PackageDiff};
///
/// let none = PackageSet::default();
/// let surface = exec_surface(Some(&none), None, &[]);
/// assert_eq!(surface.packages, PackageDiff::Unavailable);
/// assert_eq!(surface.exec_roots, [ExecRoot::Everything]);
/// ```
#[must_use]
pub fn exec_surface(
    base: Option<&PackageSet>,
    head: Option<&PackageSet>,
    declared: &[ExecRoot],
) -> ExecSurface {
    let (Some(base), Some(head)) = (base, head) else {
        return ExecSurface {
            packages: PackageDiff::Unavailable,
            exec_roots: vec![ExecRoot::Everything],
        };
    };
    let build_time = roots::build_time(base)
        .union(roots::build_time(head))
        .into_roots();
    ExecSurface {
        packages: PackageDiff::Computed(diff::changes(base, head)),
        exec_roots: roots::confined(declared, build_time, [&base.symlinks, &head.symlinks]),
    }
}
