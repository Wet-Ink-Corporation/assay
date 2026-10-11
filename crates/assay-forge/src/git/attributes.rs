//! `.gitattributes` policing with `git check-attr` (`docs/architecture/03-contracts.md` §2.1).
//!
//! Neutralising attributes does not work (the in-tree file stays in force), so they are
//! forbidden instead: on every changed path, both sides of a rename, every attribute in
//! [`POLICED_ATTRIBUTES`] must be `unspecified`. Policing by extension would not do, because
//! `#[path = "evil.txt"]`, `include!` and `build = "build.txt"` make any file Rust. `* -diff`
//! would blind the changed-line oracle and `* filter=lfs` would collapse files to pointers.
//! The only exception is a path matching an [`AttributeAllowlist`] glob from the base
//! configuration, and Rust sources, manifests and trust files can never be excepted.

use std::collections::HashSet;
use std::fmt;

use super::glob::{Glob, GlobError};
use super::{GitError, Malformed};

/// Why a set of changed paths failed attribute policing.
#[derive(Debug, thiserror::Error)]
pub enum AttributeError {
    /// A policed attribute is set, unset or valued on a changed path that the allowlist does
    /// not admit: the gate fails closed with reason `gate-error:gitattributes`.
    #[error(
        "gate-error:gitattributes: {} policed attribute(s) set on changed files",
        violations.len()
    )]
    Forbidden {
        /// Every offending path and attribute, in git's order.
        violations: Vec<PolicedAttribute>,
    },
    /// `git check-attr` could not answer, which also fails closed.
    #[error(transparent)]
    Git(#[from] GitError),
}

/// The attributes that change what git shows of a file's content or how it is converted.
pub(super) const POLICED_ATTRIBUTES: [&str; 7] = [
    "diff",
    "text",
    "filter",
    "working-tree-encoding",
    "eol",
    "crlf",
    "ident",
];

/// The only state a policed attribute may have.
const UNSPECIFIED: &str = "unspecified";

/// File names (lowercase, at any depth) that configure the build, lints or supply chain.
const TRUST_FILE_NAMES: [&str; 8] = [
    "cargo.toml",
    "cargo.lock",
    "clippy.toml",
    ".clippy.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "deny.toml",
    ".gitattributes",
];

/// Whether `path` (a `/`-separated git path) can never be admitted by an
/// [`AttributeAllowlist`].
///
/// Protected are Rust sources, manifests and lockfiles, `.cargo/` and `.assay/` contents,
/// `rust-toolchain*`, the clippy, rustfmt and cargo-deny configs, `.claude/temper*.json` and
/// `.gitattributes` itself. Names are compared without regard to ASCII case, so a
/// case-insensitive checkout cannot slip `SRC/LIB.RS` past the check.
#[must_use]
pub fn is_protected_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let (directories, file) = path.rsplit_once('/').unwrap_or(("", &path));
    let in_directory = |name: &str| directories.split('/').any(|directory| directory == name);
    file.strip_suffix(".rs").is_some()
        || TRUST_FILE_NAMES.contains(&file)
        || file.starts_with("rust-toolchain")
        || in_directory(".cargo")
        || in_directory(".assay")
        || (in_directory(".claude")
            && file.starts_with("temper")
            && file.strip_suffix(".json").is_some())
}

/// Globs of paths that may carry policed attributes, read from the base configuration.
///
/// Meant for binary assets marked `-diff`. Empty by default. Never admits a path for which
/// [`is_protected_path`] holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttributeAllowlist {
    globs: Vec<Glob>,
}

impl AttributeAllowlist {
    /// Parses `globs`. `*` matches within one path segment, `?` one character other than
    /// `/`, `**` anything and `**/` zero or more directories; a glob without `/` matches the
    /// file name at any depth, one with `/` the whole path.
    ///
    /// # Errors
    /// [`GlobError`] for the first glob that is empty, starts with `/`, uses `[`, `]`, `{`,
    /// `}`, `!` or `\`, or has a `**` that is not a whole path segment (`a**`, `**b`, `***`).
    pub fn new<I>(globs: I) -> Result<Self, GlobError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let globs = globs
            .into_iter()
            .map(|glob| glob.as_ref().parse())
            .collect::<Result<_, _>>()?;
        Ok(Self { globs })
    }

    /// Whether a policed attribute on `path` is admitted.
    #[must_use]
    pub fn admits(&self, path: &str) -> bool {
        !is_protected_path(path) && self.globs.iter().any(|glob| glob.matches(path))
    }
}

/// A policed attribute that is set, unset or given a value on a changed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicedAttribute {
    path: String,
    attribute: String,
    value: String,
}

impl PolicedAttribute {
    /// The path the attribute applies to.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The attribute: `diff`, `text`, `filter`, `working-tree-encoding`, `eol`, `crlf` or
    /// `ident`.
    #[must_use]
    pub fn attribute(&self) -> &str {
        &self.attribute
    }

    /// The state git reported: `set`, `unset` or the value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for PolicedAttribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}={}", self.path, self.attribute, self.value)
    }
}

/// The outcome of attribute policing that passed.
///
/// Paths the allowlist admitted are not trusted as shown: git may present them differently
/// from their content (a NUL-byte "binary" check would not tell, since Rust string literals
/// can carry NUL), so [`opaque`](Self::opaque) lists them for downstream gates to treat as
/// fully changed (`docs/architecture/03-contracts.md` §2.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttributeReport {
    opaque: Vec<PolicedAttribute>,
}

impl AttributeReport {
    /// Every policed attribute set on an allowlisted path, in git's order.
    #[must_use]
    pub fn opaque(&self) -> &[PolicedAttribute] {
        &self.opaque
    }

    /// The allowlisted paths that carry policed attributes, each once, in the order git first
    /// reported them. A path can appear in [`opaque`](Self::opaque) apart from its first
    /// entry (it was checked twice, say as one rename's old side and another change's path),
    /// so repeats are removed by path, not only when adjacent.
    pub fn opaque_paths(&self) -> impl Iterator<Item = &str> + '_ {
        let mut seen = HashSet::new();
        self.opaque
            .iter()
            .map(PolicedAttribute::path)
            .filter(move |path| seen.insert(*path))
    }
}

/// Splits `found` into what `allowlist` admits (the report) and what it does not (an error).
pub(super) fn judge(
    found: Vec<PolicedAttribute>,
    allowlist: &AttributeAllowlist,
) -> Result<AttributeReport, AttributeError> {
    let (opaque, violations): (Vec<_>, Vec<_>) = found
        .into_iter()
        .partition(|attribute| allowlist.admits(attribute.path()));
    if violations.is_empty() {
        Ok(AttributeReport { opaque })
    } else {
        Err(AttributeError::Forbidden { violations })
    }
}

/// Reads `git check-attr -z` output (`path NUL attribute NUL state NUL` triples) and returns
/// every triple whose state is not `unspecified`.
pub(super) fn violations(stdout: &[u8]) -> Result<Vec<PolicedAttribute>, Malformed> {
    let mut found = Vec::new();
    let Some(mut fields) = super::nul_terminated_fields(stdout)? else {
        return Ok(found);
    };
    while let Some(path) = fields.next() {
        let (Some(attribute), Some(value)) = (fields.next(), fields.next()) else {
            return Err(Malformed(
                "a check-attr record is missing its attribute or state",
            ));
        };
        if value != UNSPECIFIED {
            found.push(PolicedAttribute {
                path: path.to_owned(),
                attribute: attribute.to_owned(),
                value: value.to_owned(),
            });
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_rust_sources_manifests_and_trust_files() {
        for path in [
            "src/lib.rs",
            "crates/a/build.rs",
            "Cargo.toml",
            "crates/a/Cargo.toml",
            "Cargo.lock",
            ".cargo/config.toml",
            "crates/a/.cargo/config",
            "rust-toolchain",
            "rust-toolchain.toml",
            "clippy.toml",
            "crates/a/.clippy.toml",
            "rustfmt.toml",
            ".rustfmt.toml",
            "deny.toml",
            ".claude/temper.json",
            ".claude/temper-baseline.json",
            ".assay/plugins.toml",
            ".gitattributes",
            "SRC/LIB.RS",
            "CLIPPY.TOML",
        ] {
            assert!(is_protected_path(path), "{path} must be protected");
        }
    }

    #[test]
    fn leaves_assets_and_docs_unprotected() {
        for path in [
            "README.md",
            "assets/logo.png",
            "src/lib.rsx",
            "docs/Cargo.toml.md",
            "cargo/config.toml",
            "my-rust-toolchain",
            ".claude/settings.json",
            "temper.json",
        ] {
            assert!(!is_protected_path(path), "{path} must not be protected");
        }
    }

    #[test]
    fn the_default_allowlist_admits_nothing() {
        assert!(
            !AttributeAllowlist::default().admits("assets/logo.png"),
            "with no base config every attribute fails closed"
        );
    }

    #[test]
    fn an_allowlist_admits_matching_unprotected_paths_only() {
        let allowlist = AttributeAllowlist::new(["**"]).expect("valid globs");

        assert!(allowlist.admits("assets/logo.png"), "an asset matches **");
        assert!(
            !allowlist.admits("src/evil.rs"),
            "a Rust source is never admitted"
        );
        assert!(
            !allowlist.admits("crates/a/clippy.toml"),
            "a trust file is never admitted"
        );
    }

    #[test]
    fn opaque_paths_lists_each_path_once_even_when_repeated_apart() {
        let attribute = |path: &str, attribute: &str| PolicedAttribute {
            path: path.to_owned(),
            attribute: attribute.to_owned(),
            value: "set".to_owned(),
        };
        let found = vec![
            attribute("assets/a.png", "diff"),
            attribute("assets/a.png", "text"),
            attribute("assets/b.png", "diff"),
            attribute("assets/a.png", "diff"),
        ];
        let allowlist = AttributeAllowlist::new(["assets/**"]).expect("valid globs");

        let report = judge(found, &allowlist).expect("every path is allowlisted");

        assert_eq!(
            report.opaque_paths().collect::<Vec<_>>(),
            ["assets/a.png", "assets/b.png"],
            "a path checked twice (a rename's old side and a new file) is listed once"
        );
    }

    #[test]
    fn an_allowlist_rejects_an_unsupported_glob() {
        assert_eq!(
            AttributeAllowlist::new(["*.png", "*.[ch]"]),
            Err(GlobError::Unsupported('[')),
            "a glob outside the dialect is an error, not a silent mismatch"
        );
    }

    #[test]
    fn unspecified_attributes_pass() {
        let output = b"src/lib.rs\0diff\0unspecified\0src/lib.rs\0filter\0unspecified\0";

        assert_eq!(violations(output), Ok(Vec::new()), "nothing is set");
    }

    #[test]
    fn set_unset_and_valued_attributes_are_violations() {
        let output = b"a.rs\0diff\0unset\0a.rs\0text\0set\0a.rs\0filter\0lfs\0";

        let found = violations(output).expect("well formed");

        assert_eq!(
            found,
            [
                PolicedAttribute {
                    path: "a.rs".to_owned(),
                    attribute: "diff".to_owned(),
                    value: "unset".to_owned(),
                },
                PolicedAttribute {
                    path: "a.rs".to_owned(),
                    attribute: "text".to_owned(),
                    value: "set".to_owned(),
                },
                PolicedAttribute {
                    path: "a.rs".to_owned(),
                    attribute: "filter".to_owned(),
                    value: "lfs".to_owned(),
                },
            ],
            "every state other than unspecified fails closed"
        );
    }

    #[test]
    fn rejects_a_truncated_triple() {
        assert_eq!(
            violations(b"a.rs\0diff\0"),
            Err(Malformed(
                "a check-attr record is missing its attribute or state"
            )),
            "a path and attribute without a value must not pass"
        );
    }

    #[test]
    fn rejects_output_without_a_final_nul() {
        assert_eq!(
            violations(b"a.rs\0diff\0unset"),
            Err(Malformed("the output does not end with NUL")),
            "truncated output must not pass"
        );
    }
}
