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

use std::collections::BTreeSet;

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
enum Side {
    Absent,
    Unreadable,
    Parsed(Table),
}

impl Side {
    fn of(tree: &Tree<'_>, path: &str) -> Self {
        match (tree.object(path), tree.bytes(path)) {
            (None, _) => Self::Absent,
            (Some(_), Some(bytes)) => parse(bytes).map_or(Self::Unreadable, Self::Parsed),
            (Some(_), None) => Self::Unreadable,
        }
    }

    /// The table, when the manifest is present and parses.
    const fn parsed(&self) -> Option<&Table> {
        match self {
            Self::Parsed(table) => Some(table),
            Self::Absent | Self::Unreadable => None,
        }
    }
}

/// Steps 4b-4d over every manifest of `base` and `head`.
pub(super) fn check<'t>(base: &Tree<'t>, head: &Tree<'t>) -> ManifestChecks<'t> {
    let mut found = ManifestChecks::default();
    let (base_owner, head_owner) = (Owner::of(base), Owner::of(head));
    let excluded = excluded(&base_owner, &head_owner);
    let paths: BTreeSet<&'t str> = base
        .files(is_manifest)
        .chain(head.files(is_manifest))
        .collect();
    let empty = Table::new();
    for path in paths {
        let changed = base.object(path) != head.object(path);
        let (before, after) = match (Side::of(base, path), Side::of(head, path)) {
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
        if let Some(after) = &after {
            let is_member = !excluded.iter().any(|dir| under(path, dir));
            if is_member && is_package(after) && !inherits(after) {
                found.uninherited.push(path);
            }
        }
        if !changed {
            continue;
        }
        let roots = Roots {
            base: base_owner.root_for(path),
            head: head_owner.root_for(path),
        };
        let switches = changed_switches(before.as_ref(), after.as_ref(), &roots);
        if !switches.is_empty() {
            found.switches.push(SwitchChange {
                path,
                base: base.object(path),
                head: head.object(path),
                switches,
            });
        }
        let weakened = weakened(before.as_ref().unwrap_or(&empty), after.as_ref());
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

/// The directories the workspace manifest `exclude`s at both base and head, so a PR cannot
/// exclude a package to spare it the inheritance check.
fn excluded<'o>(base: &'o Owner<'_>, head: &Owner<'_>) -> Vec<&'o str> {
    base.excluded
        .iter()
        .filter(|dir| head.excluded.contains(*dir))
        .map(String::as_str)
        .collect()
}

/// One side's workspace manifest and the manifests it does not own.
struct Owner<'t> {
    root: Side,
    /// The directories its `[workspace] exclude` names. An entry that is not a plain relative
    /// path (absolute, holding `\` or a `..` segment) is not honoured.
    excluded: BTreeSet<String>,
    /// The directory of every other manifest that holds a `[workspace]` table, or does not
    /// parse: a workspace of its own, as far as this check can tell.
    nested: Vec<&'t str>,
}

impl<'t> Owner<'t> {
    fn of(tree: &Tree<'t>) -> Self {
        let root = Side::of(tree, ROOT);
        let excluded = root
            .parsed()
            .and_then(|manifest| manifest.get("workspace"))
            .and_then(|workspace| workspace.get("exclude"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(directory)
            .collect();
        let nested = tree
            .files(is_manifest)
            .filter(|path| {
                tree.bytes(path)
                    .and_then(parse)
                    .is_none_or(|manifest| manifest.contains_key("workspace"))
            })
            .filter_map(|path| Some(path.rsplit_once('/')?.0))
            .collect();
        Self {
            root,
            excluded,
            nested,
        }
    }

    /// The workspace manifest, when it owns the manifest at `path`: `None` when it is absent
    /// or does not parse, or `path` sits in a directory it excludes or in a nested workspace.
    fn root_for(&self, path: &str) -> Option<&Table> {
        let outside = self
            .excluded
            .iter()
            .map(String::as_str)
            .chain(self.nested.iter().copied())
            .any(|dir| under(path, dir));
        if outside { None } else { self.root.parsed() }
    }
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

/// Whether the manifest at `path` sits in `dir` or below it.
fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with('/'))
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
