//! Deterministic tiering (`docs/architecture/04-pipeline.md` §2.1 to §2.3, `05-security.md`
//! T17): from the changed files, the facts other stages computed about them and the base globs,
//! the tier, its reasons, the PR classes and the required roles of the run description (03 §4).
//!
//! Every rule fails closed: both sides of a rename are read, code is never excluded and never
//! `trivial`, a binary numstat forces `full`, and a repository's globs only add.

mod change;
mod glob;
mod lists;

use std::collections::BTreeSet;

pub use change::{
    BaselineChange, ChangeSet, ExecCause, ExecRoot, FileChange, FileKind, Lines, ManifestKey,
    ManifestTrustChange, PackageChange, PackageDiff,
};
pub use glob::{Glob, GlobError};
pub use lists::RepoGlobs;
use lists::{Lists, TEMPER_BASELINE};

use crate::tier::{PrClass, Tier};
use crate::vocabulary::closed_enum;

/// More filtered changed lines than this need a split (04 §2.3, `A-05`).
const SPLIT_ABOVE: u64 = 5_000;

/// More filtered changed lines than this are `full`.
const FULL_ABOVE: u64 = 150;

/// More filtered changed lines than this are at least `lite`.
const LITE_ABOVE: u64 = 20;

/// What the exec-surface list names when the package-set diff could not be computed.
const PACKAGES_UNAVAILABLE: &str = "package set (unavailable)";

/// The tiering of one PR head: the `plan` fields of the run description (03 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tiering {
    /// The first matching row of 04 §2.3.
    pub tier: Tier,
    /// Every condition of that row that holds, in table order.
    pub tier_reasons: Vec<TierReason>,
    /// The PR's classes, in vocabulary order.
    pub classes: Vec<PrClass>,
    /// The roles the tier requires (04 §3).
    pub required_roles: Vec<RequiredRole>,
    /// Matched trust paths, both sides of renames, and `<manifest> (<key>)` for manifest trust
    /// keys; sorted, without duplicates.
    pub trust_paths_changed: Vec<String>,
    /// Build scripts and changed files under an exec root (both sides of renames), and
    /// `<package> (<cause>)` for exec-surface packages; sorted, without duplicates.
    pub exec_surface_changed: Vec<String>,
    /// What the model sees.
    pub filtered: FilteredStats,
}

/// The changed files left after filtering (04 §2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredStats {
    /// Files not excluded.
    pub files: usize,
    /// Changed lines of the files not excluded.
    pub lines: u64,
    /// Excluded files, still named to reviewers, in change order.
    pub excluded: Vec<String>,
}

closed_enum! {
    /// Why a PR got its tier: one condition of 04 §2.3.
    pub enum TierReason {
        /// More than 5,000 filtered changed lines.
        SplitSize => "split-size",
        /// Every changed file is in the docs allowlist.
        DocsOnly => "docs-only",
        /// A trust path changed.
        TrustPath => "trust-path",
        /// Exec surface changed.
        ExecSurface => "exec-surface",
        /// More than 150 filtered changed lines.
        FullSize => "full-size",
        /// A file git reports as binary, taken as the maximum size.
        BinaryFile => "binary-file",
        /// A file matches a risk glob.
        RiskGlob => "risk-glob",
        /// 21 to 150 filtered changed lines.
        LiteSize => "lite-size",
        /// A lockfile dependency changed.
        LockfileDependency => "lockfile-dependency",
        /// A new or bumped registry package.
        RegistryPackage => "registry-package",
        /// A change under `vendor/`.
        Vendor => "vendor",
        /// Anything else: up to 20 filtered lines outside the docs allowlist.
        MicroSize => "micro-size",
    }
}

closed_enum! {
    /// A reviewer role a tier requires (04 §3 "Required roles").
    pub enum RequiredRole {
        /// Logic and edge cases (`full`).
        Bugs => "bugs",
        /// Security (`full`).
        Security => "security",
        /// Design and layering (`full`).
        Design => "design",
        /// Authority granted and checks weakened (`full`, trust path or exec surface).
        Meta => "meta",
        /// Bugs, security and design in one pass (`lite`).
        Combined => "combined",
        /// A short pass on a tiny diff (`micro`).
        CombinedMicro => "combined-micro",
    }
}

/// Tiers `changes` under the shipped globs plus the repository's `globs`. Pure and total.
///
/// # Examples
///
/// ```
/// use assay_domain::{
///     BaselineChange, ChangeSet, FileChange, Lines, PackageDiff, RepoGlobs, Tier, tier,
/// };
///
/// let changes = ChangeSet {
///     files: vec![
///         FileChange::new("src/session.rs", Lines::Counted { added: 1, removed: 1 })
///             .renamed_from("src/auth/session.rs"),
///     ],
///     manifests: Vec::new(),
///     include_targets: Vec::new(),
///     exec_roots: Vec::new(),
///     packages: PackageDiff::Computed(Vec::new()),
///     temper_baseline: BaselineChange::Other,
/// };
///
/// assert_eq!(tier(&RepoGlobs::default(), &changes).tier, Tier::Full);
/// ```
#[must_use]
pub fn tier(globs: &RepoGlobs, changes: &ChangeSet) -> Tiering {
    let lists = Lists::new(globs);
    let trust_paths_changed = trust_paths(&lists, changes);
    let exec_surface_changed = exec_surface(&lists, changes);
    let filtered = filter(&lists, &changes.files);
    let package = |wanted: fn(&PackageChange) -> bool| match &changes.packages {
        PackageDiff::Computed(packages) => packages.iter().any(wanted),
        PackageDiff::Unavailable => true,
    };
    let held: Vec<TierReason> = [
        (filtered.lines > SPLIT_ABOVE, TierReason::SplitSize),
        (docs_only(&lists, changes), TierReason::DocsOnly),
        (!trust_paths_changed.is_empty(), TierReason::TrustPath),
        (!exec_surface_changed.is_empty(), TierReason::ExecSurface),
        (filtered.lines > FULL_ABOVE, TierReason::FullSize),
        (
            changes.files.iter().any(|file| file.lines == Lines::Binary),
            TierReason::BinaryFile,
        ),
        (
            any_side(&changes.files, |path| lists.is_risk(path)),
            TierReason::RiskGlob,
        ),
        (filtered.lines > LITE_ABOVE, TierReason::LiteSize),
        (
            package(|change| matches!(change, PackageChange::Lockfile { .. })),
            TierReason::LockfileDependency,
        ),
        (
            package(|change| matches!(change, PackageChange::Registry { .. })),
            TierReason::RegistryPackage,
        ),
        (
            any_side(&changes.files, |path| lists.is_vendor(path)),
            TierReason::Vendor,
        ),
    ]
    .into_iter()
    .filter_map(|(holds, reason)| holds.then_some(reason))
    .collect();
    let (tier, tier_reasons) = first_row(&held);
    let classes: Vec<PrClass> = [
        (TierReason::TrustPath, PrClass::TrustPath),
        (TierReason::ExecSurface, PrClass::ExecSurface),
    ]
    .into_iter()
    .filter_map(|(reason, class)| held.contains(&reason).then_some(class))
    .collect();
    Tiering {
        tier,
        tier_reasons,
        required_roles: required_roles(tier, !classes.is_empty()),
        classes,
        trust_paths_changed,
        exec_surface_changed,
        filtered,
    }
}

/// The table row a condition belongs to.
const fn row(reason: TierReason) -> Tier {
    match reason {
        TierReason::SplitSize => Tier::NeedsSplit,
        TierReason::DocsOnly => Tier::Trivial,
        TierReason::TrustPath
        | TierReason::ExecSurface
        | TierReason::FullSize
        | TierReason::BinaryFile
        | TierReason::RiskGlob => Tier::Full,
        TierReason::LiteSize
        | TierReason::LockfileDependency
        | TierReason::RegistryPackage
        | TierReason::Vendor => Tier::Lite,
        TierReason::MicroSize => Tier::Micro,
    }
}

/// The first row of 04 §2.3 with a condition in `held`, and its conditions there. `trivial` is
/// also refused by a trust path, exec surface, a risk glob or any package change, whatever the
/// paths (fail closed: a repo risk glob on a docs file means review).
fn first_row(held: &[TierReason]) -> (Tier, Vec<TierReason>) {
    let blocks_trivial = |reason: &TierReason| {
        !matches!(
            reason,
            TierReason::SplitSize
                | TierReason::DocsOnly
                | TierReason::FullSize
                | TierReason::BinaryFile
                | TierReason::LiteSize
                | TierReason::MicroSize
        )
    };
    let trivial_allowed = !held.iter().any(blocks_trivial);
    [Tier::NeedsSplit, Tier::Trivial, Tier::Full, Tier::Lite]
        .into_iter()
        .filter(|&tier| tier != Tier::Trivial || trivial_allowed)
        .find_map(|tier| {
            let reasons: Vec<TierReason> = held
                .iter()
                .copied()
                .filter(|&reason| row(reason) == tier)
                .collect();
            (!reasons.is_empty()).then_some((tier, reasons))
        })
        .unwrap_or_else(|| (Tier::Micro, vec![TierReason::MicroSize]))
}

/// 04 §3: full = bugs, security, design (+ meta for a trust path or exec surface); lite =
/// combined; micro = combined-micro; no LLM for trivial or needs-split. `decide` rule 8 reads the
/// same table.
pub(crate) fn required_roles(tier: Tier, trust_class: bool) -> Vec<RequiredRole> {
    match tier {
        Tier::NeedsSplit | Tier::Trivial => Vec::new(),
        Tier::Full => {
            let mut roles = vec![
                RequiredRole::Bugs,
                RequiredRole::Security,
                RequiredRole::Design,
            ];
            if trust_class {
                roles.push(RequiredRole::Meta);
            }
            roles
        }
        Tier::Lite => vec![RequiredRole::Combined],
        Tier::Micro => vec![RequiredRole::CombinedMicro],
    }
}

fn any_side(files: &[FileChange], predicate: impl Fn(&str) -> bool) -> bool {
    files.iter().flat_map(FileChange::sides).any(predicate)
}

/// Trust-path matches on either side of every file, and manifest trust keys. Only an in-place
/// edit of the temper baseline that lowers or keeps every count is spared.
fn trust_paths(lists: &Lists<'_>, changes: &ChangeSet) -> Vec<String> {
    let spared = |file: &FileChange, path: &str| {
        path == TEMPER_BASELINE
            && file.renamed_from.is_none()
            && changes.temper_baseline == BaselineChange::LoweredOrUnchanged
    };
    let globbed = changes.files.iter().flat_map(|file| {
        file.sides()
            .filter(move |path| !spared(file, path) && lists.is_trust(path))
            .map(str::to_owned)
    });
    let manifests = changes
        .manifests
        .iter()
        .map(|change| format!("{} ({})", change.path, change.key.code()));
    globbed
        .chain(manifests)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Build scripts and files under an exec root on either side of every file, and exec-surface
/// packages.
fn exec_surface(lists: &Lists<'_>, changes: &ChangeSet) -> Vec<String> {
    let under_root = |path: &str| changes.exec_roots.iter().any(|root| root.holds(path));
    let scripts = changes
        .files
        .iter()
        .flat_map(FileChange::sides)
        .filter(|path| lists.is_build_script(path) || under_root(path))
        .map(str::to_owned);
    let packages: Vec<String> = match &changes.packages {
        PackageDiff::Computed(packages) => packages
            .iter()
            .filter_map(|change| match change {
                PackageChange::ExecSurface { package, cause } => {
                    Some(format!("{package} ({})", cause.code()))
                }
                PackageChange::Registry { .. } | PackageChange::Lockfile { .. } => None,
            })
            .collect(),
        PackageDiff::Unavailable => vec![PACKAGES_UNAVAILABLE.to_owned()],
    };
    scripts
        .chain(packages)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether there is at least one file and every file is a regular file whose every side is
/// allowlisted docs that no compile-time include names (compared ignoring ASCII case).
fn docs_only(lists: &Lists<'_>, changes: &ChangeSet) -> bool {
    let included = |path: &str| {
        changes
            .include_targets
            .iter()
            .any(|target| target.eq_ignore_ascii_case(path))
    };
    !changes.files.is_empty()
        && changes.files.iter().all(|file| {
            file.kind == FileKind::File
                && file
                    .sides()
                    .all(|path| lists.is_docs(path) && !included(path))
        })
}

/// Splits the files into excluded and counted (04 §2.1). A file is excluded when it is binary
/// or every side matches an exclusion glob, unless it is a symlink or gitlink or a side is never
/// excluded.
fn filter(lists: &Lists<'_>, files: &[FileChange]) -> FilteredStats {
    let mut stats = FilteredStats {
        files: 0,
        lines: 0,
        excluded: Vec::new(),
    };
    for file in files {
        let protected =
            file.kind != FileKind::File || file.sides().any(|path| lists.is_never_excluded(path));
        let excludable = file.sides().all(|path| lists.is_excludable(path));
        match file.lines {
            Lines::Counted { added, removed } if protected || !excludable => {
                stats.files = stats.files.saturating_add(1);
                stats.lines = stats.lines.saturating_add(added.saturating_add(removed));
            }
            Lines::Binary if protected => stats.files = stats.files.saturating_add(1),
            Lines::Counted { .. } | Lines::Binary => stats.excluded.push(file.path.clone()),
        }
    }
    stats
}
