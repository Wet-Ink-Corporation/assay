//! The wire shape of `cargo metadata --format-version 1`, reduced to the fields exec-surface
//! detection reads, and its conversion into a [`PackageSet`] (OUT-01). cargo adds fields over
//! time, so unknown ones are ignored.

use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;

use super::{Declared, Edge, Location, Package, PackageSet, PathDeclared, Source};

/// The only `cargo metadata` format this reads.
const FORMAT_VERSION: u32 = 1;

/// Why `cargo metadata` output was refused. Each side that fails becomes `None` for
/// [`exec_surface`](super::exec_surface), which fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MetadataError {
    /// The text is not the JSON shape of `cargo metadata --format-version 1`.
    #[error("cargo metadata output does not parse (line {line}, column {column})")]
    Syntax {
        /// The line of the first problem, from 1.
        line: usize,
        /// Its column, from 1.
        column: usize,
    },
    /// The output is of another format version.
    #[error("cargo metadata output is format version {found}, not {FORMAT_VERSION}")]
    Version {
        /// The version it declares.
        found: u32,
    },
    /// The resolve graph names a package the package list does not hold.
    #[error("cargo metadata's resolve graph names a package it does not list")]
    UnknownPackage,
}

#[derive(Debug, Deserialize)]
struct MetadataDto {
    packages: Vec<PackageDto>,
    resolve: Option<ResolveDto>,
    version: u32,
}

#[derive(Debug, Deserialize)]
struct PackageDto {
    name: String,
    version: String,
    id: String,
    source: Option<String>,
    manifest_path: String,
    targets: Vec<TargetDto>,
    dependencies: Vec<DependencyDto>,
}

#[derive(Debug, Deserialize)]
struct TargetDto {
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DependencyDto {
    name: String,
    source: Option<String>,
    req: String,
    kind: Option<String>,
    rename: Option<String>,
    optional: bool,
    uses_default_features: bool,
    features: Vec<String>,
    target: Option<String>,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResolveDto {
    nodes: Vec<NodeDto>,
}

#[derive(Debug, Deserialize)]
struct NodeDto {
    id: String,
    deps: Vec<NodeDepDto>,
}

#[derive(Debug, Deserialize)]
struct NodeDepDto {
    pkg: String,
    dep_kinds: Vec<DepKindDto>,
}

#[derive(Debug, Deserialize)]
struct DepKindDto {
    kind: Option<String>,
}

impl PackageSet {
    /// Reads `cargo metadata --format-version 1` output (`metadata`) produced in the copy at
    /// `root`, the absolute directory cargo ran in. Paths are made relative to `root`; one
    /// that is not under it, or not canonical, is outside the repository.
    ///
    /// # Errors
    ///
    /// [`MetadataError::Syntax`] for text that is not cargo's JSON shape,
    /// [`MetadataError::Version`] for another format version and
    /// [`MetadataError::UnknownPackage`] for a resolve graph naming an unlisted package.
    pub fn from_metadata(metadata: &str, root: &str) -> Result<Self, MetadataError> {
        let dto: MetadataDto =
            serde_json::from_str(metadata).map_err(|error| MetadataError::Syntax {
                line: error.line(),
                column: error.column(),
            })?;
        if dto.version != FORMAT_VERSION {
            return Err(MetadataError::Version { found: dto.version });
        }
        let graph = dto
            .resolve
            .map(|resolve| graph(&dto.packages, resolve))
            .transpose()?;
        let packages = dto
            .packages
            .into_iter()
            .map(|package| convert(package, root))
            .collect();
        Ok(Self { packages, graph })
    }
}

/// The resolve graph as edges between package indices; development-only edges are dropped.
fn graph(packages: &[PackageDto], resolve: ResolveDto) -> Result<Vec<Vec<Edge>>, MetadataError> {
    let index: HashMap<&str, usize> = packages
        .iter()
        .enumerate()
        .map(|(at, package)| (package.id.as_str(), at))
        .collect();
    let find = |id: &str| index.get(id).copied().ok_or(MetadataError::UnknownPackage);
    let mut edges = vec![Vec::new(); packages.len()];
    for node in resolve.nodes {
        let from = edges
            .get_mut(find(&node.id)?)
            .ok_or(MetadataError::UnknownPackage)?;
        for dep in node.deps {
            let to = find(&dep.pkg)?;
            let (normal, build) = dep.dep_kinds.iter().fold((false, false), |seen, kind| {
                match kind.kind.as_deref() {
                    None => (true, seen.1),
                    Some("build") => (seen.0, true),
                    Some("dev") => seen,
                    // A kind cargo did not name before: count it as both (fail closed).
                    Some(_) => (true, true),
                }
            });
            if normal || build {
                from.push(Edge { to, build });
            }
        }
    }
    Ok(edges)
}

/// One package, its paths made relative to `root`.
fn convert(package: PackageDto, root: &str) -> Package {
    let source = match package.source {
        Some(source) => Source::Other(source),
        None => Source::Path(
            package
                .manifest_path
                .strip_suffix("/Cargo.toml")
                .map_or(Location::Outside, |dir| location(dir, root)),
        ),
    };
    let has = |wanted: &str| {
        package
            .targets
            .iter()
            .any(|target| target.kind.iter().any(|kind| kind == wanted))
    };
    let (build_script, proc_macro) = (has("custom-build"), has("proc-macro"));
    let (build_dependencies, path_dependencies) = declarations(package.dependencies, root);
    Package {
        name: package.name,
        version: package.version,
        source,
        build_script,
        proc_macro,
        build_dependencies,
        path_dependencies,
    }
}

/// The `[build-dependencies]` declarations of `dependencies`, and those of every kind that
/// name a path, each sorted.
fn declarations(
    dependencies: Vec<DependencyDto>,
    root: &str,
) -> (Vec<Declared>, Vec<PathDeclared>) {
    let mut build_dependencies = Vec::new();
    let mut path_dependencies = Vec::new();
    for dep in dependencies {
        let path = dep.path.as_deref().map(|path| location(path, root));
        if dep.kind.as_deref() == Some("build") {
            let declared = Declared {
                path,
                name: dep.name,
                rename: dep.rename,
                req: dep.req,
                source: dep.source,
                target: dep.target,
                optional: dep.optional,
                default_features: dep.uses_default_features,
                features: dep.features,
            };
            if let Some(path) = &declared.path {
                // A build-dependency with a path is recorded in both lists, each owning its
                // copy of the shared fields.
                path_dependencies.push(PathDeclared {
                    kind: dep.kind,
                    target: declared.target.clone(),
                    name: declared
                        .rename
                        .clone()
                        .unwrap_or_else(|| declared.name.clone()),
                    path: path.clone(),
                });
            }
            build_dependencies.push(declared);
        } else if let Some(path) = path {
            path_dependencies.push(PathDeclared {
                kind: dep.kind,
                target: dep.target,
                name: dep.rename.unwrap_or(dep.name),
                path,
            });
        }
    }
    build_dependencies.sort();
    path_dependencies.sort();
    (build_dependencies, path_dependencies)
}

/// `path` relative to `root`: [`Location::Inside`] when it is `root` or a canonical path
/// under it (no empty, `.` or `..` segment), else [`Location::Outside`].
fn location(path: &str, root: &str) -> Location {
    let root = root.trim_end_matches('/');
    if path == root {
        return Location::Inside(String::new());
    }
    match path
        .strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        Some(rest)
            if rest
                .split('/')
                .all(|segment| !matches!(segment, "" | "." | "..")) =>
        {
            Location::Inside(rest.to_owned())
        }
        Some(_) | None => Location::Outside,
    }
}
