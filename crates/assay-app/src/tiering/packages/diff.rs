//! The package-set diff, base vs head (04 §2.2): see
//! [`exec_surface`](super::exec_surface) for the rules.

use assay_domain::{ExecCause, PackageChange};

use super::{Declared, Package, PackageSet, PathDeclared, Source};

/// Every change from `base` to `head`, one entry per cause: head's packages in order, then the
/// packages whose name is gone.
pub(super) fn changes(base: &PackageSet, head: &PackageSet) -> Vec<PackageChange> {
    let mut changes: Vec<PackageChange> = head
        .packages
        .iter()
        .flat_map(|package| package_changes(&base.packages, package))
        .collect();
    changes.extend(
        base.packages
            .iter()
            .filter(|old| !head.packages.iter().any(|new| new.name == old.name))
            .map(|old| PackageChange::Lockfile {
                // The change names its package, so it owns a copy of the name.
                package: old.name.clone(),
            }),
    );
    changes
}

/// The changes `package` at head makes against the packages of `base`.
fn package_changes(base: &[Package], package: &Package) -> Vec<PackageChange> {
    let same_source = || {
        base.iter()
            .filter(|old| old.name == package.name && old.source == package.source)
    };
    let unchanged = same_source().find(|old| old.version == package.version);
    let causes = causes(base, package, unchanged, same_source().next());
    // Each change names its package, so it owns a copy of the name.
    let name = || package.name.clone();
    if !causes.is_empty() {
        return causes
            .into_iter()
            .map(|cause| PackageChange::ExecSurface {
                package: name(),
                cause,
            })
            .collect();
    }
    match (unchanged, package.source.is_registry()) {
        (Some(_), _) => Vec::new(),
        (None, true) => vec![PackageChange::Registry { package: name() }],
        (None, false) => vec![PackageChange::Lockfile { package: name() }],
    }
}

/// Why `package` is exec surface, given the same package at base (`unchanged`) and the first
/// base package of the same name and source (`counterpart`, a bump when `unchanged` is
/// `None`).
fn causes(
    base: &[Package],
    package: &Package,
    unchanged: Option<&Package>,
    counterpart: Option<&Package>,
) -> Vec<ExecCause> {
    let mut causes = Vec::new();
    if unchanged.is_none() && counterpart.is_none() && source_changed(base, package) {
        causes.push(ExecCause::SourceChanged);
    }
    let (had_build_script, had_proc_macro) =
        unchanged.map_or((false, false), |old| (old.build_script, old.proc_macro));
    if package.build_script && !had_build_script {
        causes.push(ExecCause::BuildScript);
    }
    if package.proc_macro && !had_proc_macro {
        causes.push(ExecCause::ProcMacro);
    }
    let (build_before, paths_before): (&[Declared], &[PathDeclared]) = counterpart
        .map_or((&[], &[]), |old| {
            (&old.build_dependencies, &old.path_dependencies)
        });
    if package.build_dependencies != build_before {
        causes.push(ExecCause::BuildDependency);
    }
    if package
        .path_dependencies
        .iter()
        .any(|declared| !paths_before.contains(declared))
    {
        causes.push(ExecCause::PathDependency);
    }
    causes
}

/// Whether `package`, which has no base package of its name and source, changed source: its
/// name is in base from another source, or it is new from a source other than a registry or a
/// path (git), which 04 §2.2's registry reduction does not cover (fail closed).
fn source_changed(base: &[Package], package: &Package) -> bool {
    let named_in_base = base.iter().any(|old| old.name == package.name);
    let unpinned = !package.source.is_registry() && !matches!(package.source, Source::Path(_));
    named_in_base || unpinned
}
