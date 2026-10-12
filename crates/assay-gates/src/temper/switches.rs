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

use toml::{Table, Value};

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

/// The names of the switches whose values differ from `base` (`None`: a new manifest) to
/// `head`, in [`SWITCHES`] order.
///
/// A new manifest is compared with an empty one, except that inheriting the workspace's
/// `edition` or `rust-version` (`edition.workspace = true`) is not a change, and a target's
/// `required-features` and `path` are not either: no test of its can have run before.
pub(super) fn changed_switches(base: Option<&Table>, head: &Table) -> Vec<&'static str> {
    let empty = Table::new();
    let new = base.is_none();
    let base = base.unwrap_or(&empty);
    SWITCHES
        .into_iter()
        .filter(|(switch, _)| projection(base, *switch, false) != projection(head, *switch, new))
        .map(|(_, name)| name)
        .collect()
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
            changed_switches(Some(&base), &head),
            Vec::<&str>::new(),
            "compared parsed"
        );
    }

    #[test]
    fn a_strengthening_is_still_a_change() {
        let base = table("[lib]\ntest = false\n");
        let head = table("[lib]\ntest = true\n");

        assert_eq!(
            changed_switches(Some(&base), &head),
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
            changed_switches(None, &head),
            [
                "edition",
                "autotests/autobenches/autobins/autoexamples/autolib",
                "target harness"
            ],
            "its `path` hides no test that ran"
        );
    }
}
