//! Step 4b, the manifest switches of one `Cargo.toml` (`docs/architecture/03-contracts.md`
//! §2.1): the keys that decide which tests run, and what the code compiles against.
//!
//! The spec's list (`test`, `doctest`, `harness`, `autotests`, `[patch]`, `[workspace]
//! exclude`, `rust-version`, `edition`) is widened, failing closed, by `[replace]` (a
//! `[patch]` by another name), every `auto*` discovery switch (`autobins = false` drops the
//! unit tests of every binary) and a target's `required-features` and `path` (a test target
//! that needs a feature the test job does not enable, or that points at another file, stops
//! running its tests), and by workspace membership: `[workspace] members` and
//! `default-members`, the presence of a `[workspace]` table and `package.workspace` (a crate
//! leaving the workspace leaves its lints and its test job). Values are compared parsed, so
//! formatting does not count; targets are compared in file order, so reordering targets that
//! set a switch counts. A deleted manifest is compared with an empty one.

use std::collections::BTreeSet;

use toml::{Table, Value};

use super::manifests::is_package;

/// One switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Switch {
    Patch,
    Replace,
    Exclude,
    RustVersion,
    Edition,
    Discovery,
    Test,
    Doctest,
    Harness,
    RequiredFeatures,
    Path,
    Members,
    DefaultMembers,
    Workspace,
    PackageWorkspace,
}

/// Every switch, with its name in findings.
const SWITCHES: [(Switch, &str); 15] = [
    (Switch::Patch, "[patch]"),
    (Switch::Replace, "[replace]"),
    (Switch::Exclude, "[workspace] exclude"),
    (Switch::Members, "[workspace] members"),
    (Switch::DefaultMembers, "[workspace] default-members"),
    (Switch::Workspace, "[workspace] table"),
    (Switch::PackageWorkspace, "package workspace"),
    (Switch::RustVersion, "rust-version"),
    (Switch::Edition, "edition"),
    (
        Switch::Discovery,
        "autotests/autobenches/autobins/autoexamples/autolib",
    ),
    (Switch::Test, "target test"),
    (Switch::Doctest, "target doctest"),
    (Switch::Harness, "target harness"),
    (Switch::RequiredFeatures, "target required-features"),
    (Switch::Path, "target path"),
];

/// What a table that is present projects to, whatever it holds: its presence is the switch.
static PRESENT: Value = Value::Boolean(true);

/// The target-discovery switches of `[package]`.
const DISCOVERY: [&str; 5] = [
    "autotests",
    "autobenches",
    "autobins",
    "autoexamples",
    "autolib",
];

/// The dependency tables whose path dependencies are compared, in both spellings Cargo reads.
const DEPENDENCY_TABLES: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "dev_dependencies",
    "build-dependencies",
    "build_dependencies",
];

/// The names of the switches whose values differ from `base` (`None`: a new manifest) to
/// `head` (`None`: a deleted one), in [`SWITCHES`] order, then a removed path dependency, then
/// a removed package.
///
/// A new manifest is compared with an empty one, except that inheriting the workspace's
/// `edition` or `rust-version` (`edition.workspace = true`) is not a change, and a target's
/// `required-features` and `path` are not either: no test of its can have run before. A
/// deleted manifest is compared with an empty one, and deleting a package is a switch of its
/// own: its tests leave the workspace with it. A path dependency is compared by table, name
/// and path; only one that base has and head does not counts (it can take a crate and its
/// tests out of the graph). An inherited dependency (`h = { workspace = true }`) is read
/// through the `[workspace.dependencies]` of the root in `roots` on its side. When that root is
/// absent, or does not declare the dependency, the dependency still counts as a path
/// dependency, failing closed.
pub(super) fn changed_switches(
    base: Option<&Table>,
    head: Option<&Table>,
    roots: &Roots<'_>,
) -> Vec<&'static str> {
    let empty = Table::new();
    let new = base.is_none();
    let before = base.unwrap_or(&empty);
    let after = head.unwrap_or(&empty);
    let mut changed: Vec<&'static str> = SWITCHES
        .into_iter()
        .filter(|(switch, _)| projection(before, *switch, false) != projection(after, *switch, new))
        .map(|(_, name)| name)
        .collect();
    if !path_dependencies(before, roots.base).is_subset(&path_dependencies(after, roots.head)) {
        changed.push("path dependency removed");
    }
    if head.is_none() && is_package(before) {
        changed.push("package removed");
    }
    changed
}

/// One path dependency: the target `cfg` its table sits under (`None` at the top level), the
/// table (`dependencies`, `workspace.dependencies`), its name, and its path.
type PathDependency<'m> = (Option<&'m str>, &'static str, &'m str, DependencyPath);

/// Where a path dependency points, as written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum DependencyPath {
    /// Its own `path`, relative to its manifest.
    Own(String),
    /// Inherited with `workspace = true`: the root's `path` for it, relative to the root.
    Inherited(String),
    /// Inherited with `workspace = true` from a root that does not declare it, or with no
    /// root that owns the manifest; counted as a path dependency, failing closed.
    Unresolved,
}

/// The workspace manifest (`Cargo.toml` at the repository root) at base and at head, where it
/// owns the manifest being compared; `None` where it is absent, does not parse or does not own
/// it (an excluded directory, or a nested workspace).
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Roots<'r> {
    pub(super) base: Option<&'r Table>,
    pub(super) head: Option<&'r Table>,
}

/// Every path dependency of `manifest`, in its own tables, under `[target.<cfg>]` and in
/// `[workspace.dependencies]`, with inherited ones read through `root`.
fn path_dependencies<'m>(
    manifest: &'m Table,
    root: Option<&Table>,
) -> BTreeSet<PathDependency<'m>> {
    let mut tables: Vec<(Option<&str>, &'static str, &Value)> = DEPENDENCY_TABLES
        .iter()
        .filter_map(|table| Some((None, *table, manifest.get(*table)?)))
        .collect();
    let targets = lookup(manifest, &["target"]).and_then(Value::as_table);
    for (cfg, target) in targets.into_iter().flatten() {
        tables.extend(
            DEPENDENCY_TABLES
                .iter()
                .filter_map(|table| Some((Some(cfg.as_str()), *table, target.get(*table)?))),
        );
    }
    if let Some(workspace) = lookup(manifest, &["workspace", "dependencies"]) {
        tables.push((None, "workspace.dependencies", workspace));
    }
    let mut found = BTreeSet::new();
    for (cfg, table, dependencies) in tables {
        for (name, spec) in dependencies.as_table().into_iter().flatten() {
            if let Some(path) = dependency_path(name, spec, root) {
                found.insert((cfg, table, name.as_str(), path));
            }
        }
    }
    found
}

/// The path of the dependency `name` declared by `spec`, or `None` when it is not a path
/// dependency. An inherited one (`workspace = true`) is read through `root`'s
/// `[workspace.dependencies]`; [`DependencyPath::Unresolved`] when there is no `root` or it
/// does not declare `name`.
fn dependency_path(name: &str, spec: &Value, root: Option<&Table>) -> Option<DependencyPath> {
    if let Some(path) = spec.get("path") {
        return Some(DependencyPath::Own(written(path)));
    }
    if spec.get("workspace") != Some(&Value::Boolean(true)) {
        return None;
    }
    let Some(declared) = root.and_then(|root| lookup(root, &["workspace", "dependencies", name]))
    else {
        return Some(DependencyPath::Unresolved);
    };
    declared
        .get("path")
        .map(|path| DependencyPath::Inherited(written(path)))
}

/// A path as written; one that is not a string is kept in its debug form, so it still
/// compares.
fn written(path: &Value) -> String {
    path.as_str()
        .map_or_else(|| format!("{path:?}"), str::to_owned)
}

/// The values of `manifest` that `switch` covers, in a fixed order; `None` for an absent value.
/// The legacy `[project]` table is read like `[package]`.
fn projection(manifest: &Table, switch: Switch, new: bool) -> Vec<Option<&Value>> {
    let at = |path: &[&str]| lookup(manifest, path);
    let package = |key: &str| {
        [
            at(&["package", key]),
            at(&["project", key]),
            at(&["workspace", "package", key]),
        ]
    };
    match switch {
        Switch::Patch => vec![at(&["patch"])],
        Switch::Replace => vec![at(&["replace"])],
        Switch::Exclude => vec![at(&["workspace", "exclude"])],
        Switch::Members => vec![at(&["workspace", "members"])],
        Switch::DefaultMembers => vec![at(&["workspace", "default-members"])],
        Switch::Workspace => vec![at(&["workspace"]).map(|_| &PRESENT)],
        Switch::PackageWorkspace => {
            vec![at(&["package", "workspace"]), at(&["project", "workspace"])]
        }
        Switch::RustVersion => inherited(package("rust-version").into_iter().collect(), new),
        Switch::Edition => inherited(
            package("edition")
                .into_iter()
                .chain(target_keys(manifest, "edition"))
                .collect(),
            new,
        ),
        Switch::Discovery => DISCOVERY
            .iter()
            .flat_map(|key| [at(&["package", key]), at(&["project", key])])
            .collect(),
        Switch::Test => target_keys(manifest, "test"),
        Switch::Doctest => target_keys(manifest, "doctest"),
        Switch::Harness => target_keys(manifest, "harness"),
        Switch::RequiredFeatures if !new => target_keys(manifest, "required-features"),
        Switch::Path if !new => target_keys(manifest, "path"),
        Switch::RequiredFeatures | Switch::Path => Vec::new(),
    }
}

/// `values`, with each `{ workspace = true }` read as absent when `new` holds.
fn inherited(values: Vec<Option<&Value>>, new: bool) -> Vec<Option<&Value>> {
    let inherits = |value: &Value| {
        value.as_table().is_some_and(|table| {
            table.len() == 1 && table.get("workspace") == Some(&Value::Boolean(true))
        })
    };
    values
        .into_iter()
        .map(|value| value.filter(|value| !(new && inherits(value))))
        .collect()
}

/// For each target (`[lib]`, then `[[bin]]`, `[[test]]`, `[[bench]]` and `[[example]]` in
/// file order) that sets `key`: its name, then the value. A target that does not set it adds
/// nothing, so a new plain target is not a change.
fn target_keys<'m>(manifest: &'m Table, key: &str) -> Vec<Option<&'m Value>> {
    let lib = lookup(manifest, &["lib"]).into_iter();
    let arrays = ["bin", "test", "bench", "example"]
        .into_iter()
        .filter_map(|kind| lookup(manifest, &[kind]))
        .filter_map(Value::as_array)
        .flatten();
    lib.chain(arrays)
        .filter_map(|target| Some([target.get("name"), Some(target.get(key)?)]))
        .flatten()
        .collect()
}

fn lookup<'m>(manifest: &'m Table, path: &[&str]) -> Option<&'m Value> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(manifest.get(*first)?, |value, key| value.get(*key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        text.parse().expect("test manifests parse")
    }

    #[test]
    fn formatting_is_not_a_change() {
        let base = table("[lib]\ntest = false\n");
        let head = table("lib = { test = false }\n");

        assert_eq!(
            changed_switches(Some(&base), Some(&head), &Roots::default()),
            Vec::<&str>::new(),
            "compared parsed"
        );
    }

    #[test]
    fn a_strengthening_is_still_a_change() {
        let base = table("[lib]\ntest = false\n");
        let head = table("[lib]\ntest = true\n");

        assert_eq!(
            changed_switches(Some(&base), Some(&head), &Roots::default()),
            ["target test"],
            "every change is a human decision"
        );
    }

    #[test]
    fn a_new_manifest_is_held_to_the_spec_switches() {
        let head = table(
            "[package]\nname = \"x\"\nedition = \"2021\"\nautotests = false\n\
             [[test]]\nname = \"it\"\npath = \"x.rs\"\nharness = false\n",
        );

        assert_eq!(
            changed_switches(None, Some(&head), &Roots::default()),
            [
                "edition",
                "autotests/autobenches/autobins/autoexamples/autolib",
                "target harness"
            ],
            "its `path` hides no test that ran"
        );
    }

    #[test]
    fn a_moved_path_dependency_is_a_removal() {
        let base = table("[dependencies]\nh = { path = \"../h\" }\n");
        let moved = table("[dependencies]\nh = { path = \"../h2\" }\n");
        let added = table(
            "[dependencies]\nh = { path = \"../h\" }\n[target.'cfg(unix)'.dependencies]\ni = { path = \"../i\" }\n",
        );

        assert_eq!(
            (
                changed_switches(Some(&base), Some(&moved), &Roots::default()),
                changed_switches(Some(&base), Some(&added), &Roots::default())
            ),
            (vec!["path dependency removed"], Vec::new()),
            "the old path leaves the graph; an added one takes nothing out"
        );
    }

    #[test]
    fn an_inherited_dependency_is_read_through_the_workspace_root() {
        let base = table("[dependencies]\nh = { workspace = true }\nr.workspace = true\n");
        let head = table("[dependencies]\n");
        let root = table("[workspace.dependencies]\nh = { path = \"crates/h\" }\nr = \"1\"\n");
        let registry = table("[workspace.dependencies]\nh = \"1\"\nr = \"1\"\n");
        let removed = |root: Option<&Table>| {
            let roots = Roots {
                base: root,
                head: root,
            };
            changed_switches(Some(&base), Some(&head), &roots)
        };

        assert_eq!(
            (
                removed(Some(&root)),
                removed(Some(&Table::new())),
                removed(None),
                removed(Some(&registry)),
            ),
            (
                vec!["path dependency removed"],
                vec!["path dependency removed"],
                vec!["path dependency removed"],
                Vec::new(),
            ),
            "a path in the root counts; one the root does not resolve counts too (fail closed)"
        );
    }

    #[test]
    fn an_own_path_never_equals_an_inherited_one_however_it_is_spelled() {
        let base = table("[dependencies]\nh = { path = \"workspace, crates/h\" }\n");
        let head = table("[dependencies]\nh = { workspace = true }\n");
        let root = table("[workspace.dependencies]\nh = { path = \"crates/h\" }\n");
        let roots = Roots {
            base: Some(&root),
            head: Some(&root),
        };

        assert_eq!(
            changed_switches(Some(&base), Some(&head), &roots),
            ["path dependency removed"],
            "the two paths are relative to different directories"
        );
    }
}
