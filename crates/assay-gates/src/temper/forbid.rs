//! The forbid backstop's lints (#172, `docs/architecture/03-contracts.md` §2.1 step 5a): every
//! entry of base's `[workspace.lints]` at `deny` or `forbid`, minus each lint an `[[exception]]`
//! of base `.assay/exceptions.toml` names, passed to rustc as `-F` so that no attribute, however
//! a macro assembles it, can lower one (E0453).
//!
//! Every name reaches an environment variable, so the table is read exactly or refused: a lint
//! key that is not a plain name (`[a-z][a-z0-9_]*`, at most [`MAX_NAME_BYTES`] bytes), a level
//! or priority [`Entry`] cannot read, a tool other than `rust`, `clippy` and `rustdoc`, and an
//! exception lint that is not a plain lint path. Not forbidden: `warn` and `allow` entries, lint
//! groups (a forbidden group only warns about a member's `allow`, `forbidden_lint_groups`, and
//! would override base's own later `allow` of a member) and `rustdoc::` lints (rustc ignores
//! them; temper's doc run is capped). Each entry's own level decides: a higher-priority group
//! that lowers it is not modelled, which can only forbid more.

use std::collections::BTreeSet;

use toml::{Table, Value};

use super::exceptions::Allowlist;
use super::manifests::{DENY, Entry};

/// Most lints forbidden in one run.
pub(super) const MAX_FORBIDDEN: usize = 512;

/// Longest lint name, without its tool's prefix. With [`MAX_FORBIDDEN`], the encoded value
/// stays under 64 KiB, half of Linux's limit on one environment string (`MAX_ARG_STRLEN`).
const MAX_NAME_BYTES: usize = 100;

/// What separates the flags of `CARGO_ENCODED_RUSTFLAGS`.
const SEPARATOR: &str = "\u{1f}";

/// rustc's lint groups (`rustc -W help`, 1.98.1), by their `[lints.rust]` name.
const RUSTC_GROUPS: [&str; 13] = [
    "deprecated_safe",
    "future_incompatible",
    "keyword_idents",
    "let_underscore",
    "nonstandard_style",
    "refining_impl_trait",
    "rust_2018_compatibility",
    "rust_2018_idioms",
    "rust_2021_compatibility",
    "rust_2024_compatibility",
    "unknown_or_malformed_diagnostic_attributes",
    "unused",
    "warnings",
];

/// clippy's lint groups (`clippy-driver -W help`, 0.1.98), by their `[lints.clippy]` name.
const CLIPPY_GROUPS: [&str; 10] = [
    "all",
    "cargo",
    "complexity",
    "correctness",
    "nursery",
    "pedantic",
    "perf",
    "restriction",
    "style",
    "suspicious",
];

/// The lints rustc forbids in this run, each a plain lint path (`unsafe_code`,
/// `clippy::unwrap_used`), in sorted order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ForbidSet(BTreeSet<String>);

/// Why base's lints or exceptions could not give a [`ForbidSet`], with a detail that may quote
/// base (trusted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Unreadable {
    /// The root manifest: not UTF-8 or TOML, a table this cannot read exactly, or more than
    /// [`MAX_FORBIDDEN`] lints to forbid.
    Manifest(String),
    /// An `[[exception]]` lint that is not a plain lint path, so it matches nothing exactly.
    Exception(String),
}

impl ForbidSet {
    /// The set base's root manifest `manifest` (`None`: absent) and `allowlist` give. No
    /// manifest, no `[workspace]` and no `[workspace.lints]` forbid nothing.
    pub(super) fn from_base(
        manifest: Option<&[u8]>,
        allowlist: &Allowlist,
    ) -> Result<Self, Unreadable> {
        let admitted = allowlist
            .exception_lints()
            .map(|lint| {
                if is_lint_path(lint) {
                    Ok(lint)
                } else {
                    Err(Unreadable::Exception(format!(
                        "the exception lint {lint:?} is not a plain lint path, so it cannot be \
                         matched against the forbidden lints"
                    )))
                }
            })
            .collect::<Result<BTreeSet<&str>, _>>()?;
        let Some(bytes) = manifest else {
            return Ok(Self::default());
        };
        denied(bytes, &admitted)
            .map(Self)
            .map_err(Unreadable::Manifest)
    }

    /// Whether nothing is forbidden, so the backstop has nothing to run for.
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The `CARGO_ENCODED_RUSTFLAGS` value: one `-F<lint>` per lint, separated by 0x1f.
    pub(super) fn encoded(&self) -> String {
        self.0
            .iter()
            .map(|lint| format!("-F{lint}"))
            .collect::<Vec<_>>()
            .join(SEPARATOR)
    }
}

/// The deny and forbid lints of the manifest `bytes` that `admitted` does not name, refused
/// past [`MAX_FORBIDDEN`] as soon as one more would be kept.
fn denied(bytes: &[u8], admitted: &BTreeSet<&str>) -> Result<BTreeSet<String>, String> {
    let text = str::from_utf8(bytes).map_err(|_utf8| "not UTF-8".to_owned())?;
    let manifest: Table = text
        .parse()
        .map_err(|error: toml::de::Error| error.to_string())?;
    let mut forbidden = BTreeSet::new();
    for (tool, lints) in workspace_lints(&manifest)?.into_iter().flatten() {
        let groups: &[&str] = match tool.as_str() {
            "rust" => &RUSTC_GROUPS,
            "clippy" => &CLIPPY_GROUPS,
            "rustdoc" => continue,
            other => return Err(format!("`workspace.lints.{other}` is not a lint tool")),
        };
        let lints = lints
            .as_table()
            .ok_or_else(|| format!("`workspace.lints.{tool}` is not a table"))?;
        for (name, value) in lints {
            let entry = read_entry(tool, name, value)?;
            let lint = path(tool, name);
            if entry.rank < DENY || groups.contains(&name.as_str()) || admitted.contains(&*lint) {
                continue;
            }
            if forbidden.len() == MAX_FORBIDDEN {
                return Err(format!("more than {MAX_FORBIDDEN} lints to forbid"));
            }
            forbidden.insert(lint);
        }
    }
    Ok(forbidden)
}

/// Base's `[workspace.lints]` table, or `None` when it has none.
fn workspace_lints(manifest: &Table) -> Result<Option<&Table>, String> {
    let Some(workspace) = manifest.get("workspace") else {
        return Ok(None);
    };
    let workspace = workspace
        .as_table()
        .ok_or_else(|| "`workspace` is not a table".to_owned())?;
    workspace
        .get("lints")
        .map(|lints| {
            lints
                .as_table()
                .ok_or_else(|| "`workspace.lints` is not a table".to_owned())
        })
        .transpose()
}

/// The entry `value` of lint `name` under `tool`, refused unless both read exactly.
fn read_entry<'v>(tool: &str, name: &str, value: &'v Value) -> Result<Entry<'v>, String> {
    if !is_name(name) {
        return Err(format!(
            "`workspace.lints.{tool}` has the key {name:?}, not a plain lint name"
        ));
    }
    Entry::of(value).ok_or_else(|| {
        format!("`workspace.lints.{tool}.{name}` has a level or priority this cannot read")
    })
}

/// `name` under `tool`'s prefix: bare for rustc's lints, `clippy::` for clippy's.
fn path(tool: &str, name: &str) -> String {
    if tool == "rust" {
        name.to_owned()
    } else {
        format!("{tool}::{name}")
    }
}

/// Whether `name` is a plain lint name: a lowercase ASCII letter, then lowercase ASCII letters,
/// digits and `_`, at most [`MAX_NAME_BYTES`] bytes.
fn is_name(name: &str) -> bool {
    name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Whether `lint` is a plain lint path: a name, or a name under `clippy::` or `rustdoc::`.
fn is_lint_path(lint: &str) -> bool {
    let name = lint
        .strip_prefix("clippy::")
        .or_else(|| lint.strip_prefix("rustdoc::"))
        .unwrap_or(lint);
    is_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(text: &str) -> Allowlist {
        Allowlist::parse(text).expect("the test's exceptions file parses")
    }

    fn set(manifest: &str) -> Result<ForbidSet, Unreadable> {
        ForbidSet::from_base(Some(manifest.as_bytes()), &Allowlist::default())
    }

    fn lints(set: &ForbidSet) -> Vec<&str> {
        set.0.iter().map(String::as_str).collect()
    }

    const TEMPLATE: &str = r#"
[workspace]
members = ["crates/*"]

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }
non_ascii_idents = "deny"
unused = { level = "deny", priority = -1 }
unexpected_cfgs = { level = "deny", check-cfg = ["cfg(x)"] }

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
naive_bytecount = "allow"
unwrap_used = "deny"
panic = { level = "deny", priority = 1 }

[workspace.lints.rustdoc]
broken_intra_doc_links = "deny"
"#;

    #[test]
    fn forbids_each_deny_and_forbid_lint_but_no_group_warn_allow_or_rustdoc_lint() {
        let forbidden = set(TEMPLATE).expect("the template's table reads");

        assert_eq!(
            lints(&forbidden),
            [
                "clippy::panic",
                "clippy::unwrap_used",
                "non_ascii_idents",
                "unexpected_cfgs",
                "unsafe_code",
            ]
        );
    }

    #[test]
    fn encodes_one_forbid_flag_per_lint_separated_by_the_unit_separator() {
        let forbidden = set(TEMPLATE).expect("the template's table reads");

        assert_eq!(
            forbidden.encoded(),
            "-Fclippy::panic\u{1f}-Fclippy::unwrap_used\u{1f}-Fnon_ascii_idents\
             \u{1f}-Funexpected_cfgs\u{1f}-Funsafe_code"
        );
    }

    #[test]
    fn nothing_is_forbidden_without_a_manifest_workspace_or_lints_table() {
        let empty = Allowlist::default();
        let absent = ForbidSet::from_base(None, &empty);
        let package = set("[package]\nname = \"p\"\n\n[lints.rust]\nunsafe_code = \"forbid\"\n");
        let bare = set("[workspace]\nmembers = []\n");

        assert_eq!(absent, Ok(ForbidSet::default()));
        assert_eq!(package, Ok(ForbidSet::default()));
        assert_eq!(bare, Ok(ForbidSet::default()));
        assert!(ForbidSet::default().is_empty(), "an empty set runs nothing");
    }

    #[test]
    fn an_admitted_exception_lifts_its_lint_whatever_its_file() {
        let admitted = allowlist(
            "[[exception]]\npath = \"crates/x/src/lib.rs\"\nlint = \"clippy::panic\"\n\
             reason = \"r\"\n\n[[exception]]\npath = \"src/a.rs\"\nlint = \"unsafe_code\"\n\
             reason = \"r\"\n\n[[exception]]\npath = \"src/b.rs\"\nlint = \"clippy::too_many_lines\"\n\
             reason = \"r\"\n",
        );

        let forbidden = ForbidSet::from_base(Some(TEMPLATE.as_bytes()), &admitted)
            .expect("the template's table reads");

        assert_eq!(
            lints(&forbidden),
            ["clippy::unwrap_used", "non_ascii_idents", "unexpected_cfgs"]
        );
    }

    #[test]
    fn refuses_an_exception_lint_that_is_not_a_plain_lint_path() {
        for lint in [
            "Clippy::Panic",
            "clippy :: panic",
            "clippy::panic::x",
            "other::panic",
            "-Aunsafe_code",
            "",
        ] {
            let admitted = allowlist(&format!(
                "[[exception]]\npath = \"src/a.rs\"\nlint = \"{lint}\"\nreason = \"r\"\n"
            ));

            let refused = ForbidSet::from_base(Some(TEMPLATE.as_bytes()), &admitted);

            assert!(
                matches!(&refused, Err(Unreadable::Exception(detail)) if detail.contains("exception")),
                "{lint:?}: {refused:?}"
            );
        }
    }

    #[test]
    fn refuses_a_table_it_cannot_read_exactly() {
        let refusals = [
            ("not TOML", "[workspace\n"),
            ("workspace not a table", "workspace = 1\n"),
            ("lints not a table", "[workspace]\nlints = \"deny\"\n"),
            ("tool not a table", "[workspace.lints]\nrust = \"deny\"\n"),
            ("unknown tool", "[workspace.lints.miri]\nx = \"deny\"\n"),
            (
                "unknown level",
                "[workspace.lints.rust]\nx = \"force-warn\"\n",
            ),
            ("level not a string", "[workspace.lints.rust]\nx = 2\n"),
            (
                "table without level",
                "[workspace.lints.rust]\nx = { priority = 1 }\n",
            ),
            (
                "priority not an integer",
                "[workspace.lints.rust]\nx = { level = \"deny\", priority = \"1\" }\n",
            ),
            (
                "uppercase name",
                "[workspace.lints.rust]\nUnsafe_code = \"deny\"\n",
            ),
            (
                "name with a space",
                "[workspace.lints.rust]\n\"unsafe code\" = \"deny\"\n",
            ),
            (
                "name with a flag",
                "[workspace.lints.rust]\n\"-Aunsafe_code\" = \"deny\"\n",
            ),
            (
                "name with a path",
                "[workspace.lints.clippy]\n\"clippy::x\" = \"deny\"\n",
            ),
            (
                "name with a separator",
                "[workspace.lints.rust]\n\"a\\u001fb\" = \"deny\"\n",
            ),
            ("empty name", "[workspace.lints.rust]\n\"\" = \"deny\"\n"),
            (
                "a warn entry with a bad name",
                "[workspace.lints.rust]\n\"A b\" = \"warn\"\n",
            ),
        ];
        for (case, manifest) in refusals {
            let refused = set(manifest);

            assert!(
                matches!(refused, Err(Unreadable::Manifest(_))),
                "{case}: {refused:?}"
            );
        }
        let not_utf8 = ForbidSet::from_base(Some(b"\xff\xfe"), &Allowlist::default());
        assert_eq!(not_utf8, Err(Unreadable::Manifest("not UTF-8".to_owned())));
    }

    #[test]
    fn reads_a_name_of_exactly_the_longest_length_and_refuses_one_past_it() {
        let longest = "a".repeat(MAX_NAME_BYTES);
        let past = "a".repeat(MAX_NAME_BYTES + 1);

        let read = set(&format!("[workspace.lints.clippy]\n{longest} = \"deny\"\n"));
        let refused = set(&format!("[workspace.lints.clippy]\n{past} = \"deny\"\n"));

        assert_eq!(
            read.map(|forbidden| forbidden.encoded()),
            Ok(format!("-Fclippy::{longest}"))
        );
        assert!(
            matches!(&refused, Err(Unreadable::Manifest(detail)) if detail.contains("not a plain")),
            "{refused:?}"
        );
    }

    /// A clippy table of `count` deny entries, each named with `width` digits.
    fn many(count: usize, width: usize) -> String {
        use std::fmt::Write as _;
        let mut manifest = String::from("[workspace.lints.clippy]\n");
        for index in 0..count {
            writeln!(manifest, "lint_{index:0>width$} = \"deny\"")
                .expect("writing to a String cannot fail");
        }
        manifest
    }

    #[test]
    fn forbids_exactly_the_most_lints_and_refuses_one_more() {
        let most = set(&many(MAX_FORBIDDEN, 3)).expect("the cap itself is read");
        let past = set(&many(MAX_FORBIDDEN + 1, 3));

        assert_eq!(most.0.len(), MAX_FORBIDDEN);
        assert!(
            matches!(&past, Err(Unreadable::Manifest(detail)) if detail.contains("512")),
            "{past:?}"
        );
    }

    #[test]
    fn the_longest_encoding_fits_one_environment_string() {
        // 512 names of 100 bytes under `clippy::`: Linux refuses an environment string over
        // MAX_ARG_STRLEN (32 pages, 128 KiB) with E2BIG; the value stays below half of it.
        let longest = many(MAX_FORBIDDEN, MAX_NAME_BYTES - "lint_".len());

        let encoded = set(&longest).expect("the caps are met").encoded();

        assert_eq!(encoded.len(), 512 * "-Fclippy::".len() + 512 * 100 + 511);
        assert!(encoded.len() < 64 << 10, "{} bytes", encoded.len());
    }
}
