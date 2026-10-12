//! The package-set diff and the build-time path crates (04 §2.2, #65), from `cargo metadata`
//! output: what a PR adds or alters that runs at build time.

use assay_app::{ExecSurface, MetadataError, PackageSet, exec_surface};
use assay_domain::{
    BaselineChange, ChangeSet, ExecCause, ExecRoot, FileChange, Lines, PackageChange, PackageDiff,
    PrClass, RepoGlobs, tier,
};
use serde_json::{Value, json};

/// Where the fixture copies live; every manifest path is under it.
const ROOT: &str = "/scratch/copy";
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// A dependency declaration, by the name of the package it resolves to.
struct Dep {
    name: &'static str,
    kind: Option<&'static str>,
    path: Option<&'static str>,
    source: Option<&'static str>,
}

/// A package of the fixture metadata.
struct Pkg {
    name: &'static str,
    version: &'static str,
    source: Option<&'static str>,
    /// The manifest's directory, relative to [`ROOT`] for a path package.
    dir: &'static str,
    kinds: Vec<&'static str>,
    deps: Vec<Dep>,
}

impl Pkg {
    fn registry(name: &'static str, version: &'static str) -> Self {
        Self {
            name,
            version,
            source: Some(CRATES_IO),
            dir: "registry",
            kinds: vec!["lib"],
            deps: Vec::new(),
        }
    }

    fn git(name: &'static str, version: &'static str) -> Self {
        Self {
            source: Some("git+https://example.invalid/x.git#0123456789abcdef"),
            ..Self::registry(name, version)
        }
    }

    fn member(name: &'static str, dir: &'static str) -> Self {
        Self {
            name,
            version: "0.1.0",
            source: None,
            dir,
            kinds: vec!["lib"],
            deps: Vec::new(),
        }
    }

    fn version(self, version: &'static str) -> Self {
        Self { version, ..self }
    }

    fn with_build_script(mut self) -> Self {
        self.kinds.push("custom-build");
        self
    }

    fn proc_macro(self) -> Self {
        Self {
            kinds: vec!["proc-macro"],
            ..self
        }
    }

    /// Depends on the registry package `name`, as `kind`.
    fn on(mut self, name: &'static str, kind: Option<&'static str>) -> Self {
        self.deps.push(Dep {
            name,
            kind,
            path: None,
            source: Some(CRATES_IO),
        });
        self
    }

    /// Depends on the path package `name` in `dir`, as `kind`.
    fn on_path(
        mut self,
        name: &'static str,
        dir: &'static str,
        kind: Option<&'static str>,
    ) -> Self {
        self.deps.push(Dep {
            name,
            kind,
            path: Some(dir),
            source: None,
        });
        self
    }

    fn id(&self) -> String {
        match self.source {
            None => format!("path+file://{}#{}", self.manifest_dir(), self.version),
            Some(source) => format!("{source}#{}@{}", self.name, self.version),
        }
    }

    fn manifest_dir(&self) -> String {
        if self.dir.starts_with('/') {
            self.dir.to_owned()
        } else if self.dir.is_empty() {
            ROOT.to_owned()
        } else {
            format!("{ROOT}/{}", self.dir)
        }
    }
}

/// `cargo metadata --format-version 1` for `packages`, each dependency resolved to the first
/// package with its name.
fn metadata(packages: &[Pkg]) -> String {
    metadata_value(packages).to_string()
}

fn metadata_value(packages: &[Pkg]) -> Value {
    let id_of = |name: &str| {
        packages
            .iter()
            .find(|package| package.name == name)
            .map(Pkg::id)
    };
    let listed: Vec<Value> = packages
        .iter()
        .map(|package| {
            json!({
                "name": package.name,
                "version": package.version,
                "id": package.id(),
                "source": package.source,
                "manifest_path": format!("{}/Cargo.toml", package.manifest_dir()),
                "targets": package.kinds.iter().map(|kind| json!({
                    "kind": [kind],
                    "name": package.name,
                })).collect::<Vec<_>>(),
                "dependencies": package.deps.iter().map(|dep| json!({
                    "name": dep.name,
                    "source": dep.source,
                    "req": "*",
                    "kind": dep.kind,
                    "rename": null,
                    "optional": false,
                    "uses_default_features": true,
                    "features": [],
                    "target": null,
                    "path": dep.path.map(|dir| format!("{ROOT}/{dir}")),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let nodes: Vec<Value> = packages
        .iter()
        .map(|package| {
            json!({
                "id": package.id(),
                "deps": package.deps.iter().filter_map(|dep| id_of(dep.name).map(|id| json!({
                    "name": dep.name,
                    "pkg": id,
                    "dep_kinds": [{"kind": dep.kind, "target": null}],
                }))).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "packages": listed,
        "workspace_members": [],
        "resolve": {"nodes": nodes, "root": null},
        "target_directory": format!("{ROOT}/target"),
        "version": 1,
        "workspace_root": ROOT,
    })
}

fn set(packages: &[Pkg]) -> Result<PackageSet, MetadataError> {
    PackageSet::from_metadata(&metadata(packages), ROOT)
}

fn surface(base: &[Pkg], head: &[Pkg]) -> Result<ExecSurface, MetadataError> {
    Ok(exec_surface(Some(&set(base)?), Some(&set(head)?)))
}

fn packages(base: &[Pkg], head: &[Pkg]) -> Result<PackageDiff, MetadataError> {
    Ok(surface(base, head)?.packages)
}

fn exec(package: &str, cause: ExecCause) -> PackageChange {
    PackageChange::ExecSurface {
        package: package.to_owned(),
        cause,
    }
}

fn path(root: &str) -> ExecRoot {
    ExecRoot::Path(root.to_owned())
}

#[test]
fn identical_package_sets_change_nothing() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("x", "crates/x").on("serde", None),
            Pkg::registry("serde", "1.0.0"),
        ]
    };

    assert_eq!(
        surface(&packages(), &packages())?,
        ExecSurface {
            packages: PackageDiff::Computed(Vec::new()),
            exec_roots: Vec::new(),
        }
    );
    Ok(())
}

#[test]
fn new_build_script_in_a_member_is_exec_surface() -> Result<(), MetadataError> {
    let base = [Pkg::member("x", "crates/x")];
    let head = [Pkg::member("x", "crates/x").with_build_script()];

    assert_eq!(
        packages(&base, &head)?,
        PackageDiff::Computed(vec![exec("x", ExecCause::BuildScript)])
    );
    Ok(())
}

#[test]
fn bumped_registry_crate_gaining_a_build_script_is_exec_surface() -> Result<(), MetadataError> {
    let base = [Pkg::registry("foo", "1.0.0")];
    let head = [Pkg::registry("foo", "1.1.0").with_build_script()];

    assert_eq!(
        packages(&base, &head)?,
        PackageDiff::Computed(vec![exec("foo", ExecCause::BuildScript)])
    );
    Ok(())
}

#[test]
fn plain_registry_bump_is_a_registry_change_not_exec_surface() -> Result<(), MetadataError> {
    let base = [Pkg::registry("bar", "1.0.0")];
    let head = [Pkg::registry("bar", "2.0.0")];

    assert_eq!(
        packages(&base, &head)?,
        PackageDiff::Computed(vec![PackageChange::Registry {
            package: "bar".to_owned()
        }])
    );
    Ok(())
}

#[test]
fn new_registry_crate_without_build_targets_is_a_registry_change() -> Result<(), MetadataError> {
    let head = [Pkg::registry("bar", "1.0.0")];

    assert_eq!(
        packages(&[], &head)?,
        PackageDiff::Computed(vec![PackageChange::Registry {
            package: "bar".to_owned()
        }])
    );
    Ok(())
}

#[test]
fn new_proc_macro_crate_is_exec_surface() -> Result<(), MetadataError> {
    let head = [Pkg::registry("derive_x", "1.0.0").proc_macro()];

    assert_eq!(
        packages(&[], &head)?,
        PackageDiff::Computed(vec![exec("derive_x", ExecCause::ProcMacro)])
    );
    Ok(())
}

#[test]
fn registry_crate_moved_to_git_is_a_source_change() -> Result<(), MetadataError> {
    let base = [Pkg::registry("foo", "1.0.0")];
    let head = [Pkg::git("foo", "1.0.0")];

    assert_eq!(
        packages(&base, &head)?,
        PackageDiff::Computed(vec![exec("foo", ExecCause::SourceChanged)])
    );
    Ok(())
}

#[test]
fn new_git_crate_is_exec_surface() -> Result<(), MetadataError> {
    // Fail closed (Weigh-In default): only a registry checks what it serves against the
    // lockfile's checksum, so a new git package is a source change, not a registry one.
    let head = [Pkg::git("foo", "1.0.0")];

    assert_eq!(
        packages(&[], &head)?,
        PackageDiff::Computed(vec![exec("foo", ExecCause::SourceChanged)])
    );
    Ok(())
}

#[test]
fn changed_build_dependencies_are_exec_surface() -> Result<(), MetadataError> {
    let base = [Pkg::member("x", "crates/x")];
    let head = [
        Pkg::member("x", "crates/x").on("cc", Some("build")),
        Pkg::registry("cc", "1.0.0"),
    ];

    assert_eq!(
        packages(&base, &head)?,
        PackageDiff::Computed(vec![
            exec("x", ExecCause::BuildDependency),
            PackageChange::Registry {
                package: "cc".to_owned()
            },
        ])
    );
    Ok(())
}

#[test]
fn added_path_dependency_is_exec_surface() -> Result<(), MetadataError> {
    let base = [Pkg::member("x", "crates/x"), Pkg::member("y", "crates/y")];
    let head = [
        Pkg::member("x", "crates/x").on_path("y", "crates/y", None),
        Pkg::member("y", "crates/y"),
    ];

    assert_eq!(
        packages(&base, &head)?,
        PackageDiff::Computed(vec![exec("x", ExecCause::PathDependency)])
    );
    Ok(())
}

#[test]
fn removed_package_is_a_lockfile_change() -> Result<(), MetadataError> {
    let base = [Pkg::registry("old", "1.0.0")];

    assert_eq!(
        packages(&base, &[])?,
        PackageDiff::Computed(vec![PackageChange::Lockfile {
            package: "old".to_owned()
        }])
    );
    Ok(())
}

#[test]
fn member_with_a_build_script_bumping_its_version_is_exec_surface() -> Result<(), MetadataError> {
    // Fail closed (Weigh-In default): 04 §2.2 makes every bumped package with a build script
    // exec surface, and a path package is no exception.
    let base = [Pkg::member("x", "crates/x").with_build_script()];
    let head = [Pkg::member("x", "crates/x")
        .version("0.2.0")
        .with_build_script()];

    assert_eq!(
        packages(&base, &head)?,
        PackageDiff::Computed(vec![exec("x", ExecCause::BuildScript)])
    );
    Ok(())
}

#[test]
fn path_build_dependency_is_an_exec_root() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("x", "crates/x").on_path("helpers", "crates/helpers", Some("build")),
            Pkg::member("helpers", "crates/helpers"),
        ]
    };

    assert_eq!(
        surface(&packages(), &packages())?.exec_roots,
        vec![path("crates/helpers")]
    );
    Ok(())
}

#[test]
fn proc_macro_and_its_path_dependencies_are_exec_roots() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("derive", "crates/derive").proc_macro().on_path(
                "helpers",
                "crates/helpers",
                None,
            ),
            Pkg::member("helpers", "crates/helpers"),
        ]
    };

    assert_eq!(
        surface(&packages(), &packages())?.exec_roots,
        vec![path("crates/derive"), path("crates/helpers")]
    );
    Ok(())
}

#[test]
fn build_time_closure_is_transitive() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("x", "crates/x").on_path("gen", "crates/gen", Some("build")),
            Pkg::member("gen", "crates/gen")
                .on_path("model", "crates/model", None)
                .on_path("tool", "crates/tool", Some("build")),
            Pkg::member("model", "crates/model"),
            Pkg::member("tool", "crates/tool"),
        ]
    };

    assert_eq!(
        surface(&packages(), &packages())?.exec_roots,
        vec![
            path("crates/gen"),
            path("crates/model"),
            path("crates/tool")
        ]
    );
    Ok(())
}

#[test]
fn dev_dependency_of_a_build_dependency_is_not_followed() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("x", "crates/x").on_path("gen", "crates/gen", Some("build")),
            Pkg::member("gen", "crates/gen").on_path("fixtures", "crates/fixtures", Some("dev")),
            Pkg::member("fixtures", "crates/fixtures"),
        ]
    };

    assert_eq!(
        surface(&packages(), &packages())?.exec_roots,
        vec![path("crates/gen")]
    );
    Ok(())
}

#[test]
fn normal_path_dependency_of_a_plain_crate_is_not_an_exec_root() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("x", "crates/x").on_path("y", "crates/y", None),
            Pkg::member("y", "crates/y"),
        ]
    };

    assert_eq!(surface(&packages(), &packages())?.exec_roots, Vec::new());
    Ok(())
}

#[test]
fn registry_build_dependency_is_not_an_exec_root() -> Result<(), MetadataError> {
    // Its content is pinned by the lockfile's checksum; a change to it is a package change.
    let packages = || {
        [
            Pkg::member("x", "crates/x").on("cc", Some("build")),
            Pkg::registry("cc", "1.0.0"),
        ]
    };

    assert_eq!(surface(&packages(), &packages())?.exec_roots, Vec::new());
    Ok(())
}

#[test]
fn exec_roots_of_both_sides_are_kept() -> Result<(), MetadataError> {
    let base = [
        Pkg::member("x", "crates/x").on_path("old", "crates/old", Some("build")),
        Pkg::member("old", "crates/old"),
    ];
    let head = [
        Pkg::member("x", "crates/x").on_path("new", "crates/new", Some("build")),
        Pkg::member("new", "crates/new"),
    ];

    assert_eq!(
        surface(&base, &head)?.exec_roots,
        vec![path("crates/new"), path("crates/old")]
    );
    Ok(())
}

#[test]
fn root_package_reached_at_build_time_is_everything() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("x", "crates/x").on_path("top", "", Some("build")),
            Pkg::member("top", ""),
        ]
    };

    assert_eq!(
        surface(&packages(), &packages())?.exec_roots,
        vec![ExecRoot::Everything]
    );
    Ok(())
}

#[test]
fn path_crate_outside_the_copy_reached_at_build_time_is_everything() -> Result<(), MetadataError> {
    let packages = || {
        [
            Pkg::member("x", "crates/x").on_path("far", "../far", Some("build")),
            Pkg::member("far", "/elsewhere/far"),
        ]
    };

    assert_eq!(
        surface(&packages(), &packages())?.exec_roots,
        vec![ExecRoot::Everything]
    );
    Ok(())
}

#[test]
fn metadata_without_a_resolve_graph_is_everything() -> Result<(), MetadataError> {
    let mut value = metadata_value(&[Pkg::member("x", "crates/x")]);
    value["resolve"] = Value::Null;
    let without = PackageSet::from_metadata(&value.to_string(), ROOT)?;

    let surface = exec_surface(Some(&without), Some(&without));

    assert_eq!(surface.exec_roots, vec![ExecRoot::Everything]);
    Ok(())
}

#[test]
fn unavailable_side_is_exec_surface_everywhere() -> Result<(), MetadataError> {
    let head = set(&[Pkg::member("x", "crates/x")])?;

    assert_eq!(
        exec_surface(None, Some(&head)),
        ExecSurface {
            packages: PackageDiff::Unavailable,
            exec_roots: vec![ExecRoot::Everything],
        }
    );
    Ok(())
}

#[test]
fn a_tree_without_manifests_is_an_empty_set() -> Result<(), MetadataError> {
    let head = [Pkg::registry("bar", "1.0.0")];

    assert_eq!(
        exec_surface(Some(&PackageSet::default()), Some(&set(&head)?)).packages,
        PackageDiff::Computed(vec![PackageChange::Registry {
            package: "bar".to_owned()
        }])
    );
    Ok(())
}

#[test]
fn metadata_of_another_format_version_is_refused() {
    let mut value = metadata_value(&[]);
    value["version"] = json!(2);

    let result = PackageSet::from_metadata(&value.to_string(), ROOT);

    assert_eq!(result, Err(MetadataError::Version { found: 2 }));
}

#[test]
fn unreadable_metadata_is_refused() {
    let result = PackageSet::from_metadata("{\"packages\": [", ROOT);

    assert!(
        matches!(result, Err(MetadataError::Syntax { line: 1, .. })),
        "got {result:?}"
    );
}

#[test]
fn resolve_naming_an_unlisted_package_is_refused() {
    let mut value = metadata_value(&[Pkg::member("x", "crates/x")]);
    value["resolve"]["nodes"][0]["deps"] = json!([{
        "name": "ghost",
        "pkg": "path+file:///nowhere#0.1.0",
        "dep_kinds": [{"kind": null, "target": null}],
    }]);

    let result = PackageSet::from_metadata(&value.to_string(), ROOT);

    assert_eq!(result, Err(MetadataError::UnknownPackage));
}

#[test]
fn path_build_dependency_edited_in_place_is_exec_surface() -> Result<(), MetadataError> {
    // Moved from assay-domain's `path_build_dependency_edited_in_place_awaits_issue_65` (#50):
    // a path crate reachable as a build-dependency runs at build time, so an edit to it in
    // place, which no package-set diff sees, is exec surface through its exec root.
    let packages = || {
        [
            Pkg::member("x", "crates/x").on_path(
                "codegen-helpers",
                "crates/codegen-helpers",
                Some("build"),
            ),
            Pkg::member("codegen-helpers", "crates/codegen-helpers"),
        ]
    };
    let surface = surface(&packages(), &packages())?;
    let changes = ChangeSet {
        files: vec![FileChange::new(
            "crates/codegen-helpers/src/lib.rs",
            Lines::Counted {
                added: 1,
                removed: 0,
            },
        )],
        manifests: Vec::new(),
        include_targets: Vec::new(),
        exec_roots: surface.exec_roots,
        packages: surface.packages,
        temper_baseline: BaselineChange::Other,
    };

    let tiering = tier(&RepoGlobs::default(), &changes);

    assert_eq!(tiering.classes, vec![PrClass::ExecSurface]);
    Ok(())
}
