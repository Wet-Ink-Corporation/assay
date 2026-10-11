//! `.gitattributes` policing with `git check-attr` (`docs/architecture/03-contracts.md` §2.1).
//!
//! Neutralising attributes does not work (the in-tree file stays in force), so they are
//! forbidden instead: on the policed paths every attribute in [`POLICED_ATTRIBUTES`] must be
//! `unspecified`. `* -diff` would blind the changed-line oracle and `* filter=lfs` would
//! collapse files to pointers.

use std::fmt;

use super::{GitError, Malformed};

/// Why a set of changed paths failed attribute policing.
#[derive(Debug, thiserror::Error)]
pub enum AttributeError {
    /// A policed attribute is set, unset or valued on a policed path: the gate fails closed
    /// with reason `gate-error:gitattributes`.
    #[error(
        "gate-error:gitattributes: {} policed attribute(s) set on changed files",
        violations.len()
    )]
    Forbidden {
        /// Every offending path and attribute, in git's order.
        violations: Vec<AttributeViolation>,
    },
    /// `git check-attr` could not answer, which also fails closed.
    #[error(transparent)]
    Git(#[from] GitError),
}

/// The attributes that change what git shows of a file's content.
pub(super) const POLICED_ATTRIBUTES: [&str; 4] =
    ["diff", "text", "filter", "working-tree-encoding"];

/// The only state a policed attribute may have.
const UNSPECIFIED: &str = "unspecified";

/// Whether the attributes of `path` (a `/`-separated git path) are policed.
///
/// Policed are `.rs` files, `Cargo.toml` files, anything under a `.cargo/` directory and
/// `rust-toolchain*` files. Names are compared without regard to ASCII case, so a case-insensitive checkout cannot
/// slip `SRC/LIB.RS` past the check.
#[must_use]
pub fn is_policed_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let (directories, file) = path.rsplit_once('/').unwrap_or(("", &path));
    file.strip_suffix(".rs").is_some()
        || file == "cargo.toml"
        || file.starts_with("rust-toolchain")
        || directories
            .split('/')
            .any(|directory| directory == ".cargo")
}

/// A policed attribute that is set, unset or given a value on a policed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeViolation {
    path: String,
    attribute: String,
    value: String,
}

impl AttributeViolation {
    /// The path the attribute applies to.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The attribute, one of `diff`, `text`, `filter` and `working-tree-encoding`.
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

impl fmt::Display for AttributeViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}={}", self.path, self.attribute, self.value)
    }
}

/// Reads `git check-attr -z` output (`path NUL attribute NUL state NUL` triples) and returns
/// every triple whose state is not `unspecified`.
pub(super) fn violations(stdout: &[u8]) -> Result<Vec<AttributeViolation>, Malformed> {
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
            found.push(AttributeViolation {
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
    fn polices_rust_sources_manifests_cargo_config_and_toolchain_files() {
        for path in [
            "src/lib.rs",
            "crates/a/build.rs",
            "Cargo.toml",
            "crates/a/Cargo.toml",
            ".cargo/config.toml",
            "crates/a/.cargo/config",
            "rust-toolchain",
            "rust-toolchain.toml",
            "SRC/LIB.RS",
            "cargo.toml",
        ] {
            assert!(is_policed_path(path), "{path} must be policed");
        }
    }

    #[test]
    fn leaves_other_files_alone() {
        for path in [
            "README.md",
            "Cargo.lock",
            "src/lib.rsx",
            "docs/Cargo.toml.md",
            "cargo/config.toml",
            "my-rust-toolchain",
        ] {
            assert!(!is_policed_path(path), "{path} must not be policed");
        }
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
                AttributeViolation {
                    path: "a.rs".to_owned(),
                    attribute: "diff".to_owned(),
                    value: "unset".to_owned(),
                },
                AttributeViolation {
                    path: "a.rs".to_owned(),
                    attribute: "text".to_owned(),
                    value: "set".to_owned(),
                },
                AttributeViolation {
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
        assert!(
            violations(b"a.rs\0diff\0").is_err(),
            "a path and attribute without a value must not pass"
        );
    }
}
