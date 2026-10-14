//! Steps 4b-4d (`docs/architecture/03-contracts.md` §2.1) over every `Cargo.toml` of base and
//! head, parsed in-process: manifest switches that change, a lints table that weakens, and a
//! package that does not inherit the workspace lints.
//!
//! Fail closed where the reading is not certain: a manifest that does not parse is reported
//! on its own; a lint level, priority or entry shape this check does not know counts as a
//! weakening when it changes; an added lint below `deny` counts too, since it can lower a lint
//! a group set higher; and every package outside a directory that base and head both
//! `exclude` must inherit, whether or not `members` names it (Cargo makes path dependencies
//! members too).

use std::collections::{BTreeMap, BTreeSet};

use toml::{Table, Value};

use super::head::is_manifest;
use super::statics::Tree;
use super::switches::{Roots, changed_switches};

/// The workspace manifest's path.
const ROOT: &str = "Cargo.toml";

/// What steps 4b-4d found, each list by path.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ManifestChecks<'t> {
    /// Manifests that do not parse, with whether head holds them (base only, otherwise).
    pub(super) unreadable: Vec<(&'t str, bool)>,
    /// Manifests whose switches changed.
    pub(super) switches: Vec<SwitchChange<'t>>,
    /// Manifests whose lints tables weakened, with each weakening in words, and whether head
    /// holds them.
    pub(super) weakened: Vec<(&'t str, bool, Vec<String>)>,
    /// Package manifests at head that do not inherit the workspace lints.
    pub(super) uninherited: Vec<&'t str>,
}

/// One manifest whose switches changed, with the blob ids that bind an allowlist entry to this
/// exact change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SwitchChange<'t> {
    pub(super) path: &'t str,
    pub(super) base: Option<&'t str>,
    pub(super) head: Option<&'t str>,
    pub(super) switches: Vec<&'static str>,
}

/// `bytes` as a TOML table, or `None` when it is not UTF-8 or not TOML.
fn parse(bytes: &[u8]) -> Option<Table> {
    str::from_utf8(bytes).ok()?.parse().ok()
}

/// A manifest on one side: absent, unparseable, or parsed.
#[derive(Clone, Copy)]
enum Side<'m> {
    Absent,
    Unreadable,
    Parsed(&'m Table),
}

/// Steps 4b-4d over every manifest of `base` and `head`, each parsed once per side ([`Owner`]).
pub(super) fn check<'t>(base: &Tree<'t>, head: &Tree<'t>) -> ManifestChecks<'t> {
    let mut found = ManifestChecks::default();
    let paths: Vec<&'t str> = base
        .files(is_manifest)
        .chain(head.files(is_manifest))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let (base_owner, head_owner) = (Owner::of(base), Owner::of(head));
    let placed = placement(&paths, &base_owner, &head_owner);
    let empty = Table::new();
    for (path, (excluded, base_outside, head_outside)) in paths.into_iter().zip(placed) {
        let changed = base.object(path) != head.object(path);
        let (before, after) = match (base_owner.side(path), head_owner.side(path)) {
            (_, Side::Unreadable) => {
                found.unreadable.push((path, true));
                continue;
            }
            (Side::Unreadable, _) => {
                found.unreadable.push((path, false));
                continue;
            }
            (Side::Absent, Side::Absent) => continue,
            (Side::Absent, Side::Parsed(after)) => (None, Some(after)),
            (Side::Parsed(before), Side::Absent) => (Some(before), None),
            (Side::Parsed(before), Side::Parsed(after)) => (Some(before), Some(after)),
        };
        if let Some(after) = after
            && !excluded
            && is_package(after)
            && !inherits(after)
        {
            found.uninherited.push(path);
        }
        if !changed {
            continue;
        }
        let roots = Roots {
            base: base_owner.root_unless(base_outside),
            head: head_owner.root_unless(head_outside),
        };
        let switches = changed_switches(before, after, &roots);
        if !switches.is_empty() {
            found.switches.push(SwitchChange {
                path,
                base: base.object(path),
                head: head.object(path),
                switches,
            });
        }
        let weakened = weakened(before.unwrap_or(&empty), after);
        if !weakened.is_empty() {
            found.weakened.push((path, after.is_some(), weakened));
        }
    }
    found.uninherited.sort_unstable();
    found
}

/// Whether `manifest` declares a package (`[package]`, or the legacy `[project]`).
pub(super) fn is_package(manifest: &Table) -> bool {
    manifest.contains_key("package") || manifest.contains_key("project")
}

/// Whether `manifest` carries `[lints] workspace = true`.
fn inherits(manifest: &Table) -> bool {
    manifest
        .get("lints")
        .and_then(|lints| lints.get("workspace"))
        == Some(&Value::Boolean(true))
}

/// One side's manifests, each parsed once, and the manifests its workspace manifest does not
/// own.
struct Owner<'t> {
    /// Every manifest of the side, by path; `None` when it was not read or does not parse.
    manifests: BTreeMap<&'t str, Option<Table>>,
    /// The directories its `[workspace] exclude` names. An entry that is not a plain relative
    /// path (absolute, holding `\` or a `..` segment) is not honoured.
    excluded: BTreeSet<String>,
    /// The directory of every other manifest that holds a `[workspace]` table, or does not
    /// parse: a workspace of its own, as far as this check can tell.
    nested: Vec<&'t str>,
}

impl<'t> Owner<'t> {
    fn of(tree: &Tree<'t>) -> Self {
        let manifests: BTreeMap<&'t str, Option<Table>> = tree
            .files(is_manifest)
            .map(|path| (path, tree.bytes(path).and_then(parse)))
            .collect();
        let excluded = manifests
            .get(ROOT)
            .and_then(Option::as_ref)
            .and_then(|manifest| manifest.get("workspace"))
            .and_then(|workspace| workspace.get("exclude"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(directory)
            .collect();
        let nested = manifests
            .iter()
            .filter(|(_, manifest)| {
                manifest
                    .as_ref()
                    .is_none_or(|manifest| manifest.contains_key("workspace"))
            })
            .filter_map(|(path, _)| Some(path.rsplit_once('/')?.0))
            .collect();
        Self {
            manifests,
            excluded,
            nested,
        }
    }

    /// The manifest at `path` on this side.
    fn side(&self, path: &str) -> Side<'_> {
        match self.manifests.get(path) {
            None => Side::Absent,
            Some(None) => Side::Unreadable,
            Some(Some(manifest)) => Side::Parsed(manifest),
        }
    }

    /// Every directory whose manifests this side's root does not own: those it excludes, and
    /// nested workspaces.
    fn outside(&self) -> impl Iterator<Item = &str> {
        self.excluded
            .iter()
            .map(String::as_str)
            .chain(self.nested.iter().copied())
    }

    /// The workspace manifest, unless the manifest being compared is `outside` it (below a
    /// directory of [`Self::outside`]); `None` too when it is absent or does not parse.
    fn root_unless(&self, outside: bool) -> Option<&Table> {
        if outside {
            None
        } else {
            self.manifests.get(ROOT).and_then(Option::as_ref)
        }
    }
}

/// For each of `paths`, sorted: whether it sits below a directory that base and head both
/// exclude, so a PR cannot exclude a package to spare it the inheritance check; and whether it
/// is outside base's root, then head's ([`Owner::outside`]).
fn placement(paths: &[&str], base: &Owner<'_>, head: &Owner<'_>) -> Vec<(bool, bool, bool)> {
    let excluded_both = base
        .excluded
        .iter()
        .filter(|dir| head.excluded.contains(*dir))
        .map(String::as_str);
    let excluded = below_any(paths, excluded_both);
    let base_outside = below_any(paths, base.outside());
    let head_outside = below_any(paths, head.outside());
    excluded
        .into_iter()
        .zip(base_outside)
        .zip(head_outside)
        .map(|((excluded, base), head)| (excluded, base, head))
        .collect()
}

/// For each of `paths`, sorted, whether it sits below one of `dirs`. The paths below `dir`
/// are those starting `dir/`: one contiguous run of the sorted paths, from `dir/` up to `dir0`
/// (`0` follows `/`), found by binary search. O((paths + dirs) log paths), never paths times
/// dirs (H-13).
fn below_any<'d>(paths: &[&str], dirs: impl IntoIterator<Item = &'d str>) -> Vec<bool> {
    let mut runs: Vec<(usize, usize)> = dirs
        .into_iter()
        .map(|dir| {
            let (from, to) = (format!("{dir}/"), format!("{dir}0"));
            (
                paths.partition_point(|path| *path < from.as_str()),
                paths.partition_point(|path| *path < to.as_str()),
            )
        })
        .collect();
    runs.sort_unstable();
    let mut runs = runs.into_iter().peekable();
    let mut covered_until = 0;
    (0..paths.len())
        .map(|index| {
            while let Some((_, end)) = runs.next_if(|(start, _)| *start <= index) {
                covered_until = covered_until.max(end);
            }
            index < covered_until
        })
        .collect()
}

/// `entry` as a canonical directory, or `None` when it is not a plain relative path.
fn directory(entry: &str) -> Option<String> {
    if entry.starts_with('/') || entry.contains('\\') {
        return None;
    }
    let segments: Vec<&str> = entry
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    let plain = !segments.is_empty() && !segments.contains(&"..");
    plain.then(|| segments.join("/"))
}

/// Every weakening of the `[lints]` and `[workspace.lints]` tables from `base` to `head`
/// (`None`: the manifest is gone, which removes its workspace lints; a package's own lints go
/// with its package).
fn weakened(base: &Table, head: Option<&Table>) -> Vec<String> {
    fn workspace_lints(manifest: &Table) -> Option<&Value> {
        manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("lints"))
    }
    let mut found = Vec::new();
    if let Some(head) = head {
        found.extend(lints_weakened(
            "lints",
            base.get("lints"),
            head.get("lints"),
        ));
    }
    found.extend(lints_weakened(
        "workspace.lints",
        workspace_lints(base),
        head.and_then(workspace_lints),
    ));
    found
}

/// Every weakening from the lints table `base` to `head`, named under `table`.
fn lints_weakened(table: &str, base: Option<&Value>, head: Option<&Value>) -> Vec<String> {
    let empty = Table::new();
    let base = base.and_then(Value::as_table).unwrap_or(&empty);
    let head = head.and_then(Value::as_table).unwrap_or(&empty);
    let mut found = Vec::new();
    for (key, before) in base {
        let after = head.get(key);
        match (before.as_table(), after.and_then(Value::as_table)) {
            (Some(before), after) => {
                let after = after.unwrap_or(&empty);
                found.extend(tool_weakened(&format!("{table}.{key}"), before, after));
            }
            (None, _) if after != Some(before) => found.push(format!("`{table}.{key}` changed")),
            (None, _) => {}
        }
    }
    for (key, after) in head.iter().filter(|(key, _)| !base.contains_key(*key)) {
        match after.as_table() {
            Some(after) => found.extend(tool_weakened(&format!("{table}.{key}"), &empty, after)),
            None if key == "workspace" && after == &Value::Boolean(true) => {}
            None => found.push(format!("`{table}.{key}` added")),
        }
    }
    found
}

/// Every weakening of one tool's lints (`[lints.clippy]`), named under `tool`.
fn tool_weakened(tool: &str, base: &Table, head: &Table) -> Vec<String> {
    let removed = base
        .iter()
        .filter_map(|(lint, before)| match head.get(lint) {
            None => Some(format!("`{tool}.{lint}` removed")),
            Some(after) => {
                lowered(before, after).then(|| format!("`{tool}.{lint}` lowered or changed"))
            }
        });
    let added = head
        .iter()
        .filter(|(lint, _)| !base.contains_key(*lint))
        .filter(|(_, after)| Entry::of(after).is_none_or(|entry| entry.rank < DENY))
        .map(|(lint, _)| format!("`{tool}.{lint}` added below `deny`"));
    removed.chain(added).collect()
}

/// The rank of `deny`; `allow` and `warn` rank below it, `forbid` above.
const DENY: u8 = 2;

/// One lint's entry: its level's rank, its priority and any other keys (`check-cfg`).
#[derive(Debug, PartialEq)]
struct Entry<'v> {
    rank: u8,
    priority: i64,
    rest: Vec<(&'v String, &'v Value)>,
}

impl<'v> Entry<'v> {
    /// `value` as an entry, or `None` for a shape or level this check does not know.
    fn of(value: &'v Value) -> Option<Self> {
        let (level, table) = match value {
            Value::String(level) => (level.as_str(), None),
            Value::Table(table) => (table.get("level")?.as_str()?, Some(table)),
            _ => return None,
        };
        let rank = ["allow", "warn", "deny", "forbid"]
            .iter()
            .position(|known| *known == level)
            .and_then(|rank| u8::try_from(rank).ok())?;
        let priority = match table.and_then(|table| table.get("priority")) {
            None => 0,
            Some(priority) => priority.as_integer()?,
        };
        let rest = table
            .into_iter()
            .flatten()
            .filter(|(key, _)| !matches!(key.as_str(), "level" | "priority"))
            .collect();
        Some(Self {
            rank,
            priority,
            rest,
        })
    }
}

/// Whether the entry `after` is weaker than `before`: a lower level, another priority or other
/// keys; any change to an entry this check cannot read counts.
fn lowered(before: &Value, after: &Value) -> bool {
    match (Entry::of(before), Entry::of(after)) {
        (Some(before), Some(after)) => {
            after.rank < before.rank
                || after.priority != before.priority
                || after.rest != before.rest
        }
        _ => before != after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revision::{EntryKind, TreeEntry};

    /// A regular file at `path` holding `text`; its object id is the text, so equal texts
    /// compare equal.
    fn file(path: &str, text: &str) -> (TreeEntry, Vec<u8>) {
        let entry = TreeEntry {
            path: path.to_owned(),
            kind: EntryKind::File,
            object: text.to_owned(),
            size: None,
        };
        (entry, text.as_bytes().to_vec())
    }

    fn tree(files: &[(TreeEntry, Vec<u8>)]) -> Tree<'_> {
        Tree::new(
            files
                .iter()
                .map(|(entry, bytes)| (entry, Some(bytes.as_slice()))),
        )
    }

    #[test]
    fn many_nested_workspaces_are_placed_in_one_sorted_pass() {
        let root = "[workspace]\n\n[workspace.dependencies]\nhelper = \"1\"\n";
        let member = |dependencies: &str| {
            format!(
                "[package]\nname = \"m\"\n\n[lints]\nworkspace = true\n\n[dependencies]\n\
                 {dependencies}"
            )
        };
        let (before, after) = (member("helper = { workspace = true }\n"), member(""));
        let mut base = vec![file("Cargo.toml", root), file("m/Cargo.toml", &before)];
        let mut head = vec![file("Cargo.toml", root), file("m/Cargo.toml", &after)];
        for index in 0..100_000 {
            let nested = file(&format!("n{index}/Cargo.toml"), "[workspace]\n");
            base.push(nested.clone());
            head.push(nested);
        }
        for index in 0..10_000 {
            base.push(file(&format!("n{index}/m/Cargo.toml"), &before));
            head.push(file(&format!("n{index}/m/Cargo.toml"), &after));
        }

        let found = check(&tree(&base), &tree(&head));

        assert_eq!(
            (
                found.switches.len(),
                found
                    .switches
                    .iter()
                    .all(|change| change.path.starts_with('n'))
            ),
            (10_000, true),
            "below a nested workspace `helper` is unresolved; the root resolves its own member's"
        );
    }

    #[test]
    fn a_path_is_below_a_directory_only_past_its_slash() {
        let paths = [
            "a/Cargo.toml",
            "a/b/Cargo.toml",
            "a0/Cargo.toml",
            "ab/Cargo.toml",
            "b",
        ];

        assert_eq!(
            (
                below_any(&paths, ["a"]),
                below_any(&paths, ["a/b", "a"]),
                below_any(&paths, ["c"])
            ),
            (
                vec![true, true, false, false, false],
                vec![true, true, false, false, false],
                vec![false; 5]
            ),
            "`a0` and `ab` sort after `a/` but are not below `a`"
        );
    }
}
