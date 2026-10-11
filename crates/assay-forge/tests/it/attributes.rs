//! `.gitattributes` on any changed path fails closed unless a base-config glob allows it
//! (`03 §2.1`, threat T5).

use std::error::Error;

use assay_forge::git::{AttributeAllowlist, AttributeError};

use crate::support::{Fixture, fixture, fixture_with_removals};

fn police(
    fixture: &Fixture,
    allowlist: &AttributeAllowlist,
) -> Result<Result<(), AttributeError>, Box<dyn Error>> {
    let changes = fixture.changes()?;
    Ok(fixture
        .repo
        .police_attributes(&fixture.head, &changes, allowlist))
}

/// The policed attributes git reported, as `path: attribute=value`.
fn forbidden(result: Result<(), AttributeError>) -> Result<Vec<String>, Box<dyn Error>> {
    match result {
        Err(AttributeError::Forbidden { violations }) => {
            Ok(violations.iter().map(ToString::to_string).collect())
        }
        other => Err(format!("expected a gitattributes gate error, got {other:?}").into()),
    }
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn passes_when_no_changed_path_has_a_policed_attribute() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n"),
        (".gitattributes", b"*.md merge=union\n"),
    ])?;

    let result = police(&fixture, &AttributeAllowlist::default())?;

    assert!(
        matches!(result, Ok(())),
        "attributes outside the policed set are allowed, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn star_minus_diff_fails_closed() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n"),
        (".gitattributes", b"* -diff\n"),
    ])?;

    let result = police(&fixture, &AttributeAllowlist::default())?;

    assert_eq!(
        forbidden(result)?,
        [".gitattributes: diff=unset", "src/lib.rs: diff=unset"],
        "`* -diff` blinds the changed-line oracle"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn star_filter_lfs_fails_closed() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("Cargo.toml", b"[package]\nname = \"renamed\"\n"),
        (".gitattributes", b"Cargo.toml filter=lfs\n"),
    ])?;

    let result = police(&fixture, &AttributeAllowlist::default())?;

    assert_eq!(
        forbidden(result)?,
        ["Cargo.toml: filter=lfs"],
        "`filter=lfs` collapses manifests to pointers"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_rust_module_hidden_under_another_extension_fails_closed() -> Result<(), Box<dyn Error>> {
    // `#[path = "evil.txt"] mod evil;` compiles evil.txt as Rust.
    let fixture = fixture(&[
        ("src/lib.rs", b"#[path = \"evil.txt\"]\nmod evil;\n"),
        ("src/evil.txt", b"pub fn evil() {}\n"),
        (".gitattributes", b"*.txt -diff\n"),
    ])?;

    let result = police(&fixture, &AttributeAllowlist::default())?;

    assert_eq!(
        forbidden(result)?,
        ["src/evil.txt: diff=unset"],
        "every changed path is policed, whatever its extension"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn eol_crlf_and_ident_are_policed() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n"),
        (".gitattributes", b"src/*.rs eol=crlf crlf ident\n"),
    ])?;

    let result = police(&fixture, &AttributeAllowlist::default())?;

    assert_eq!(
        forbidden(result)?,
        [
            "src/lib.rs: eol=crlf",
            "src/lib.rs: crlf=set",
            "src/lib.rs: ident=set"
        ],
        "line-ending and keyword expansion rewrite what the oracle sees"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn both_sides_of_a_rename_are_policed() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_removals(
        &[
            ("src/kept.rs", b"pub fn kept() {}\n"),
            (".gitattributes", b"legacy/** -diff\n"),
        ],
        &["legacy/kept.rs"],
    )?;

    let result = police(&fixture, &AttributeAllowlist::default())?;

    assert_eq!(
        forbidden(result)?,
        ["legacy/kept.rs: diff=unset"],
        "the base side of a rename is policed too"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_base_allowlist_glob_admits_binary_assets() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("assets/logo.png", b"\x89PNG\0binary"),
        (".gitattributes", b"*.png -diff\n"),
    ])?;
    let allowlist = AttributeAllowlist::new(["*.png"])?;

    let result = police(&fixture, &allowlist)?;

    assert!(
        matches!(result, Ok(())),
        "an allowlisted binary asset may carry -diff, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn trust_files_can_never_be_allowlisted() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("clippy.toml", b"too-many-arguments-threshold = 99\n"),
        ("Cargo.lock", b"version = 4\n"),
        (".claude/temper.json", b"{}\n"),
        ("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n"),
        ("logo.png", b"\x89PNG"),
        (
            ".gitattributes",
            b"*.toml -diff\n*.lock -diff\n*.json -diff\n*.rs -diff\n*.png -diff\n",
        ),
    ])?;
    let allowlist = AttributeAllowlist::new(["**"])?;

    let result = police(&fixture, &allowlist)?;

    assert_eq!(
        forbidden(result)?,
        [
            ".claude/temper.json: diff=unset",
            "Cargo.lock: diff=unset",
            "clippy.toml: diff=unset",
            "src/lib.rs: diff=unset"
        ],
        "a glob matching everything still cannot admit trust files or Rust sources"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_error_names_the_gitattributes_gate_error() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        (
            "rust-toolchain.toml",
            b"[toolchain]\nchannel = \"nightly\"\n",
        ),
        (
            ".gitattributes",
            b"rust-toolchain* working-tree-encoding=ISO-8859-1\n",
        ),
    ])?;

    let result = police(&fixture, &AttributeAllowlist::default())?;

    let message = result.err().map(|error| error.to_string());
    assert_eq!(
        message.as_deref(),
        Some("gate-error:gitattributes: 1 policed attribute(s) set on changed files"),
        "the reason code is what the decision publishes"
    );
    Ok(())
}
