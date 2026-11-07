//! The ADR-07 layering policy (`docs/architecture/02-architecture.md` §2) and a check of the
//! workspace's `cargo metadata` against it.

use std::error::Error;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Each of the six crates with the workspace crates it may depend on: domain <- app <-
/// {forge, runner, gates} <- cli. Adding a crate or an edge here is a crate-boundary change (H-16).
const LAYERS: [(&str, &[&str]); 6] = [
    ("review-domain", &[]),
    ("review-app", &["review-domain"]),
    ("review-forge", &["review-domain", "review-app"]),
    ("review-runner", &["review-domain", "review-app"]),
    ("review-gates", &["review-domain", "review-app"]),
    (
        "review-cli",
        &[
            "review-domain",
            "review-app",
            "review-forge",
            "review-runner",
            "review-gates",
        ],
    ),
];

const DOMAIN: &str = "review-domain";

/// The only third-party crates the domain may use outside its tests (02 §2).
const DOMAIN_ALLOWED: [&str; 3] = ["serde", "thiserror", "sha2"];

/// assay drives weigh-in and temper as external tools and never links their code (P8). A crate is
/// theirs when its name, with `_` read as `-`, is one of these or one of these followed by `-`
/// (`temper`, `temper-scan`, `weighin_core`); `temperature` and `weighing` are not.
const FORBIDDEN_FAMILIES: [&str; 3] = ["weighin", "weigh-in", "temper"];

const WORKSPACE_PREFIX: &str = "review-";

/// Where Review's crates live, relative to the workspace root: `crates/review-<x>/` in this
/// repository and `crates/review/<pkg>/` once imported into the redkiln monorepo. Only members
/// under these are checked, so a sibling plugin's crates in the same workspace are neither
/// unknown members here nor Review's to police; a seventh crate of Review's own still is.
const REVIEW_ROOTS: [&str; 2] = ["crates/review-", "crates/review/"];

/// How a package uses a dependency, as `cargo metadata` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Normal,
    Dev,
    Build,
}

/// One entry of a package's `[dependencies]`, `[dev-dependencies]` or `[build-dependencies]`.
#[derive(Debug)]
pub(crate) struct Dependency {
    pub(crate) name: String,
    pub(crate) kind: Kind,
}

/// A workspace member and what it declares.
#[derive(Debug)]
pub(crate) struct Package {
    pub(crate) name: String,
    pub(crate) publishable: bool,
    pub(crate) dependencies: Vec<Dependency>,
}

/// A way the workspace departs from ADR-07, borrowing the names from the checked packages.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Violation<'a> {
    /// A member that is not one of the six crates.
    UnknownMember { package: &'a str },
    /// One of the six crates is not a member.
    MissingMember { package: &'a str },
    /// A workspace edge that does not point inward.
    ForbiddenWorkspaceEdge { from: &'a str, to: &'a str },
    /// A non-test domain dependency other than serde, thiserror and sha2.
    DomainDependencyNotAllowed { dependency: &'a str },
    /// A dependency on weigh-in or temper code.
    ForbiddenCrate {
        package: &'a str,
        dependency: &'a str,
    },
    /// A crate that `cargo publish` would accept.
    Publishable { package: &'a str },
}

/// Every departure from ADR-07 in `packages`: missing crates first, then in member order.
pub(crate) fn violations(packages: &[Package]) -> Vec<Violation<'_>> {
    let mut found: Vec<Violation<'_>> = LAYERS
        .iter()
        .filter(|(name, _)| !packages.iter().any(|package| package.name == *name))
        .map(|&(name, _)| Violation::MissingMember { package: name })
        .collect();
    for package in packages {
        package_violations(package, &mut found);
    }
    found
}

fn package_violations<'a>(package: &'a Package, found: &mut Vec<Violation<'a>>) {
    let Some(allowed) = allowed_workspace_dependencies(&package.name) else {
        found.push(Violation::UnknownMember {
            package: &package.name,
        });
        return;
    };
    if package.publishable {
        found.push(Violation::Publishable {
            package: &package.name,
        });
    }
    for dependency in &package.dependencies {
        let name = dependency.name.as_str();
        if is_forbidden(name) {
            found.push(Violation::ForbiddenCrate {
                package: &package.name,
                dependency: name,
            });
        } else if name.starts_with(WORKSPACE_PREFIX) {
            if !allowed.contains(&name) {
                found.push(Violation::ForbiddenWorkspaceEdge {
                    from: &package.name,
                    to: name,
                });
            }
        } else if package.name == DOMAIN
            && dependency.kind != Kind::Dev
            && !DOMAIN_ALLOWED.contains(&name)
        {
            found.push(Violation::DomainDependencyNotAllowed { dependency: name });
        }
    }
}

fn allowed_workspace_dependencies(package: &str) -> Option<&'static [&'static str]> {
    LAYERS
        .iter()
        .find(|(name, _)| *name == package)
        .map(|&(_, allowed)| allowed)
}

fn is_forbidden(dependency: &str) -> bool {
    let normalized = dependency.replace('_', "-");
    FORBIDDEN_FAMILIES.iter().any(|family| {
        normalized
            .strip_prefix(family)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
    })
}

/// Whether a member whose manifest is at `manifest`, relative to the workspace root and with `/`
/// separators, is one of Review's crates.
pub(crate) fn is_review_member(manifest: &str) -> bool {
    REVIEW_ROOTS.iter().any(|root| {
        manifest
            .strip_prefix(root)
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(dir, file)| !dir.is_empty() && file == "Cargo.toml")
    })
}

/// Review's workspace members, read from `cargo metadata --no-deps` and kept by
/// [`is_review_member`]. The workspace is found from this crate's own manifest, so the check
/// works wherever the crates sit under the workspace root.
pub(crate) fn workspace_packages() -> Result<Vec<Package>, Box<dyn Error>> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cargo metadata failed: {stderr}").into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let root = metadata
        .get("workspace_root")
        .and_then(Value::as_str)
        .ok_or("cargo metadata has no `workspace_root`")?;
    let mut packages = Vec::new();
    for value in metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata has no `packages` array")?
    {
        let manifest = string_field(value, "manifest_path")?;
        let relative = Path::new(&manifest)
            .strip_prefix(root)
            .map_err(|_| format!("member manifest {manifest} is outside {root}"))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| format!("member manifest {manifest} is not UTF-8"))?
            .replace('\\', "/");
        if is_review_member(&relative) {
            packages.push(parse_package(value)?);
        }
    }
    Ok(packages)
}

fn parse_package(value: &Value) -> Result<Package, Box<dyn Error>> {
    let name = string_field(value, "name")?;
    // `publish` is null when any registry is allowed, and `[]` for `publish = false`.
    let publishable = !value
        .get("publish")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    let dependencies = value
        .get("dependencies")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("package {name} has no `dependencies` array"))?
        .iter()
        .map(parse_dependency)
        .collect::<Result<_, _>>()?;
    Ok(Package {
        name,
        publishable,
        dependencies,
    })
}

fn parse_dependency(value: &Value) -> Result<Dependency, Box<dyn Error>> {
    let name = string_field(value, "name")?;
    // `kind` is null for a normal dependency.
    let kind = match value.get("kind").and_then(Value::as_str) {
        None => Kind::Normal,
        Some("dev") => Kind::Dev,
        Some("build") => Kind::Build,
        Some(other) => return Err(format!("dependency {name} has unknown kind {other}").into()),
    };
    Ok(Dependency { name, kind })
}

fn string_field(value: &Value, field: &str) -> Result<String, Box<dyn Error>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("cargo metadata entry has no string `{field}`").into())
}
