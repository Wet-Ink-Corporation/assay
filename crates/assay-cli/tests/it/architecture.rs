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
    let adapter_deps = [("assay-domain", Kind::Normal), ("assay-app", Kind::Normal)];
    vec![
        package(
            "assay-domain",
            &[
                ("serde", Kind::Normal),
                ("thiserror", Kind::Normal),
                ("sha2", Kind::Normal),
                ("proptest", Kind::Dev),
            ],
        ),
        package("assay-app", &[("assay-domain", Kind::Normal)]),
        package("assay-forge", &adapter_deps),
        package("assay-runner", &adapter_deps),
        package("assay-gates", &adapter_deps),
        package(
            "assay-cli",
            &[
                ("assay-domain", Kind::Normal),
                ("assay-app", Kind::Normal),
                ("assay-forge", Kind::Normal),
                ("assay-runner", Kind::Normal),
                ("assay-gates", Kind::Normal),
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
            "assay-domain",
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
        package("assay-domain", &[("cc", Kind::Build)]),
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
            "assay-app",
            &[
                ("assay-domain", Kind::Normal),
                ("assay-forge", Kind::Normal),
            ],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::ForbiddenWorkspaceEdge {
            from: "assay-app",
            to: "assay-forge"
        }],
        "app must not depend outward on an adapter"
    );
}

#[test]
fn rejects_domain_depending_on_app_even_for_tests() {
    let workspace = replace(
        fully_wired_workspace(),
        package("assay-domain", &[("assay-app", Kind::Dev)]),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::ForbiddenWorkspaceEdge {
            from: "assay-domain",
            to: "assay-app"
        }],
        "a dev-dependency edge still points outward"
    );
}

#[test]
fn rejects_adapter_depending_on_a_sibling_adapter() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "assay-gates",
            &[("assay-app", Kind::Normal), ("assay-runner", Kind::Normal)],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![Violation::ForbiddenWorkspaceEdge {
            from: "assay-gates",
            to: "assay-runner"
        }],
        "adapters must not depend on each other"
    );
}

#[test]
fn rejects_dependency_on_weighin_or_temper_code() {
    let workspace = replace(
        fully_wired_workspace(),
        package(
            "assay-gates",
            &[("weighin-core", Kind::Normal), ("temper_scan", Kind::Dev)],
        ),
    );

    assert_eq!(
        violations(&workspace),
        vec![
            Violation::ForbiddenCrate {
                package: "assay-gates",
                dependency: "weighin-core"
            },
            Violation::ForbiddenCrate {
                package: "assay-gates",
                dependency: "temper_scan"
            },
        ],
        "assay drives weigh-in and temper as tools and never links them (P8)"
    );
}

#[test]
fn rejects_publishable_crate() {
    let mut runner = package("assay-runner", &[]);
    runner.publishable = true;
    let workspace = replace(fully_wired_workspace(), runner);

    assert_eq!(
        violations(&workspace),
        vec![Violation::Publishable {
            package: "assay-runner"
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
        .filter(|member| member.name != "assay-runner")
        .collect();

    assert_eq!(
        violations(&workspace),
        vec![Violation::MissingMember {
            package: "assay-runner"
        }],
        "all six crates of ADR-07 must exist"
    );
}
