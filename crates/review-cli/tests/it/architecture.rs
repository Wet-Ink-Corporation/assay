//! The crate graph follows ADR-07 and `docs/architecture/02-architecture.md` §2: six crates,
//! dependencies pointing inward, a domain that depends only on serde, thiserror and sha2, no
//! weighin-core or temper code, and nothing publishable.

use crate::layering::{Dependency, Kind, Package, Violation, violations, workspace_packages};

fn package(name: &str, dependencies: &[(&str, Kind)]) -> Package {
    Package {
        name: name.to_owned(),
        publishable: false,
        dependencies: dependencies
            .iter()
            .map(|&(dependency, kind)| Dependency {
                name: dependency.to_owned(),
                kind,
            })
            .collect(),
    }
}

/// The six crates with every allowed workspace edge and the whole domain allowlist.
fn fully_wired_workspace() -> Vec<Package> {
    let adapter_deps = [
        ("review-domain", Kind::Normal),
        ("review-app", Kind::Normal),
    ];
    vec![
        package(
            "review-domain",
            &[
                ("serde", Kind::Normal),
                ("thiserror", Kind::Normal),
                ("sha2", Kind::Normal),
                ("proptest", Kind::Dev),
            ],
        ),
        package("review-app", &[("review-domain", Kind::Normal)]),
        package("review-forge", &adapter_deps),
        package("review-runner", &adapter_deps),
        package("review-gates", &adapter_deps),
        package(
            "review-cli",
            &[
                ("review-domain", Kind::Normal),
                ("review-app", Kind::Normal),
                ("review-forge", Kind::Normal),
                ("review-runner", Kind::Normal),
                ("review-gates", Kind::Normal),
                ("serde_json", Kind::Dev),
            ],
        ),
    ]
}

/// `workspace` with the member named like `replacement` swapped for it, or with it appended.
fn replace(mut workspace: Vec<Package>, replacement: Package) -> Vec<Package> {
    match workspace
        .iter_mut()
        .find(|member| member.name == replacement.name)
    {
        Some(slot) => *slot = replacement,
        None => workspace.push(replacement),
    }
    workspace
}

#[test]
#[cfg_attr(miri, ignore = "spawns `cargo metadata`; Miri cannot run processes")]
fn this_workspace_respects_the_layering() -> Result<(), Box<dyn std::error::Error>> {
    let packages = workspace_packages()?;

    assert_eq!(
        violations(&packages),
        Vec::new(),
        "the workspace breaks ADR-07 layering"
    );
    Ok(())
}

#[test]
fn accepts_every_allowed_edge() {
    let workspace = fully_wired_workspace();

    assert_eq!(
        violations(&workspace),
        Vec::new(),
        "allowed edges were rejected"
    );
}

#[test]
fn rejects_domain_dependency_outside_allowlist() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "review-domain",
            &[("serde", Kind::Normal), ("tokio", Kind::Normal)],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::DomainDependencyNotAllowed {
            dependency: "tokio"
        }],
        "a domain dependency outside serde, thiserror and sha2 must be rejected"
    );
}

#[test]
fn rejects_domain_build_dependency_outside_allowlist() {
    let workspace = replace(
        fully_wired_workspace(),
        package("review-domain", &[("cc", Kind::Build)]),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::DomainDependencyNotAllowed { dependency: "cc" }],
        "build dependencies of the domain are held to the same allowlist"
    );
}

#[test]
fn rejects_app_depending_on_an_adapter() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "review-app",
            &[
                ("review-domain", Kind::Normal),
                ("review-forge", Kind::Normal),
            ],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::ForbiddenWorkspaceEdge {
            from: "review-app",
            to: "review-forge"
        }],
        "app must not depend outward on an adapter"
    );
}

#[test]
fn rejects_domain_depending_on_app_even_for_tests() {
    let workspace = replace(
        fully_wired_workspace(),
        package("review-domain", &[("review-app", Kind::Dev)]),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::ForbiddenWorkspaceEdge {
            from: "review-domain",
            to: "review-app"
        }],
        "a dev-dependency edge still points outward"
    );
}

#[test]
fn rejects_adapter_depending_on_a_sibling_adapter() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "review-gates",
            &[
                ("review-app", Kind::Normal),
                ("review-runner", Kind::Normal),
            ],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::ForbiddenWorkspaceEdge {
            from: "review-gates",
            to: "review-runner"
        }],
        "adapters must not depend on each other"
    );
}

#[test]
fn rejects_dependency_on_weighin_or_temper_code() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "review-gates",
            &[("weighin-core", Kind::Normal), ("temper_scan", Kind::Dev)],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![
            Violation::ForbiddenCrate {
                package: "review-gates",
                dependency: "weighin-core"
            },
            Violation::ForbiddenCrate {
                package: "review-gates",
                dependency: "temper_scan"
            },
        ],
        "assay drives weigh-in and temper as tools and never links them (P8)"
    );
}

#[test]
fn accepts_crates_whose_names_only_begin_with_weighin_or_temper() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "review-gates",
            &[
                ("review-app", Kind::Normal),
                ("temperature", Kind::Normal),
                ("weighing", Kind::Normal),
                ("weigh-intake", Kind::Dev),
            ],
        ),
    );

    assert_eq!(
        violations(&workspace),
        Vec::new(),
        "only `temper`, `weighin` and `weigh-in` themselves or their `-`/`_` families are P8 crates"
    );
}

#[test]
fn rejects_the_bare_temper_crate() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "review-gates",
            &[("review-app", Kind::Normal), ("temper", Kind::Normal)],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::ForbiddenCrate {
            package: "review-gates",
            dependency: "temper"
        }],
        "the temper crate itself is temper code (P8)"
    );
}

#[test]
fn rejects_publishable_crate() {
    let mut runner = package("review-runner", &[]);
    runner.publishable = true;
    let workspace = replace(fully_wired_workspace(), runner);

    assert_eq!(
        violations(&workspace),
        vec![Violation::Publishable {
            package: "review-runner"
        }],
        "every crate stays private (publish = false)"
    );
}

#[test]
fn rejects_unknown_workspace_member() {
    let workspace = replace(fully_wired_workspace(), package("assay-events", &[]));

    assert_eq!(
        violations(&workspace),
        vec![Violation::UnknownMember {
            package: "assay-events"
        }],
        "a seventh crate is a crate-boundary change (ADR-07, H-16)"
    );
}

#[test]
fn rejects_missing_workspace_member() {
    let workspace: Vec<Package> = fully_wired_workspace()
        .into_iter()
        .filter(|member| member.name != "review-runner")
        .collect();

    assert_eq!(
        violations(&workspace),
        vec![Violation::MissingMember {
            package: "review-runner"
        }],
        "all six crates of ADR-07 must exist"
    );
}
