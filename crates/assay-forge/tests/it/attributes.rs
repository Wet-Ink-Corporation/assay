//! `.gitattributes` on changed Rust and manifest files fail closed (`03 §2.1`, threat T5).

use std::error::Error;

use assay_forge::git::{AttributeError, DiffOutcome, FileChange};

use crate::support::{Fixture, fixture};

/// The paths the head commit changed, as numstat reports them.
fn changed_paths(fixture: &Fixture) -> Result<Vec<String>, Box<dyn Error>> {
    let outcome = fixture.repo.numstat(
        &fixture.base,
        &fixture.head,
        std::time::Duration::from_secs(60),
    )?;
    let DiffOutcome::Measured(changes) = outcome else {
        return Err("the fixture diff did not finish".into());
    };
    Ok(changes
        .iter()
        .map(FileChange::path)
        .map(str::to_owned)
        .collect())
}

fn police(fixture: &Fixture) -> Result<Result<(), AttributeError>, Box<dyn Error>> {
    let changed = changed_paths(fixture)?;
    Ok(fixture
        .repo
        .police_attributes(&fixture.head, changed.iter().map(String::as_str)))
}

/// The policed attributes git reported, as `path attribute=value`.
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
fn passes_when_no_policed_attribute_is_set() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n"),
        (".gitattributes", b"*.md text\n"),
    ])?;

    let result = police(&fixture)?;

    assert!(
        matches!(result, Ok(())),
        "attributes on unpoliced paths are allowed, got {result:?}"
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

    let result = police(&fixture)?;

    assert_eq!(
        forbidden(result)?,
        ["src/lib.rs: diff=unset"],
        "`* -diff` blinds the changed-line oracle on Rust sources"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn star_filter_lfs_fails_closed() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("Cargo.toml", b"[package]\nname = \"renamed\"\n"),
        (".gitattributes", b"* filter=lfs\n"),
    ])?;

    let result = police(&fixture)?;

    assert_eq!(
        forbidden(result)?,
        ["Cargo.toml: filter=lfs"],
        "`* filter=lfs` collapses manifests to pointers"
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

    let result = police(&fixture)?;

    let message = result.err().map(|error| error.to_string());
    assert_eq!(
        message.as_deref(),
        Some("gate-error:gitattributes: 1 policed attribute(s) set on changed files"),
        "the reason code is what the decision publishes"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn unpoliced_changes_are_not_checked() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("README.md", b"# changed\n"),
        (".gitattributes", b"* -diff\n"),
    ])?;

    let result = police(&fixture)?;

    assert!(
        matches!(result, Ok(())),
        "only Rust sources, manifests and toolchain files are policed, got {result:?}"
    );
    Ok(())
}
