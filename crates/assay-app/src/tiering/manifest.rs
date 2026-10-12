//! Which trust keys of a `Cargo.toml` differ between base and head (04 §2.2).

use std::iter;

use assay_domain::ManifestKey;
use toml::{Table, Value};

/// The trust keys that differ between the `base` and `head` text of one manifest.
///
/// Keys come in [`ManifestKey::ALL`] order. A missing side (an added or removed manifest) reads as an empty
/// manifest, so every trust key it sets counts as changed. Values are compared parsed, so
/// formatting and key order do not count, and only the keys of 04 §2.2 are read.
///
/// Fails closed: if either side does not parse, the result is
/// `[ManifestKey::Unparseable]`.
///
/// # Examples
///
/// ```
/// use assay_app::manifest_trust_keys;
/// use assay_domain::ManifestKey;
///
/// let base = "[package]\nname = \"x\"\nedition = \"2024\"\n";
/// let head = "[package]\nname = \"x\"\nedition = \"2021\"\n";
/// assert_eq!(manifest_trust_keys(Some(base), Some(head)), [ManifestKey::Edition]);
/// ```
#[must_use]
pub fn manifest_trust_keys(base: Option<&str>, head: Option<&str>) -> Vec<ManifestKey> {
    let (Ok(base), Ok(head)) = (parse(base), parse(head)) else {
        return vec![ManifestKey::Unparseable];
    };
    ManifestKey::ALL
        .into_iter()
        .filter(|&key| projection(&base, key) != projection(&head, key))
        .collect()
}

fn parse(text: Option<&str>) -> Result<Table, toml::de::Error> {
    text.map_or_else(|| Ok(Table::new()), str::parse)
}

/// The values of `manifest` that `key` covers, in a fixed order; `None` for an absent value.
fn projection(manifest: &Table, key: ManifestKey) -> Vec<Option<&Value>> {
    let at = |path: &[&str]| lookup(manifest, path);
    match key {
        ManifestKey::Lints => vec![at(&["lints"])],
        ManifestKey::WorkspaceLints => vec![at(&["workspace", "lints"])],
        ManifestKey::Patch => vec![at(&["patch"])],
        ManifestKey::Replace => vec![at(&["replace"])],
        ManifestKey::WorkspaceMembers => vec![at(&["workspace", "members"])],
        ManifestKey::WorkspaceExclude => vec![at(&["workspace", "exclude"])],
        ManifestKey::RustVersion => vec![
            at(&["package", "rust-version"]),
            at(&["workspace", "package", "rust-version"]),
        ],
        ManifestKey::Edition => [
            at(&["package", "edition"]),
            at(&["workspace", "package", "edition"]),
        ]
        .into_iter()
        .chain(target_keys(manifest, &["edition"]))
        .collect(),
        ManifestKey::TestSwitch => [
            at(&["package", "autotests"]),
            at(&["package", "autobenches"]),
        ]
        .into_iter()
        .chain(target_keys(manifest, &["test", "doctest", "harness"]))
        .collect(),
        ManifestKey::Unparseable => Vec::new(),
    }
}

/// For each target (`[lib]`, then `[[bin]]`, `[[test]]`, `[[bench]]` and `[[example]]` in file
/// order) that sets any of `keys`: its name, then the value of each key. A target that sets
/// none adds nothing, so a new plain target is not a change.
fn target_keys<'a>(manifest: &'a Table, keys: &[&str]) -> Vec<Option<&'a Value>> {
    let lib = lookup(manifest, &["lib"]).into_iter();
    let arrays = ["bin", "test", "bench", "example"]
        .into_iter()
        .filter_map(|kind| lookup(manifest, &[kind]))
        .filter_map(Value::as_array)
        .flatten();
    lib.chain(arrays)
        .flat_map(|target| {
            let values: Vec<Option<&Value>> = keys.iter().map(|key| target.get(*key)).collect();
            if values.iter().all(Option::is_none) {
                return Vec::new();
            }
            iter::once(target.get("name")).chain(values).collect()
        })
        .collect()
}

fn lookup<'a>(manifest: &'a Table, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(manifest.get(*first)?, |value, key| value.get(*key))
}
