//! `DirArtifacts` (#60 ST-14, D12 A): the run's artifacts as `download-artifact` lays them out,
//! one directory per artifact, read as regular files under the caller's limit.

use std::assert_matches;
use std::error::Error;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use assay_app::{Fetched, RunArtifacts};
use assay_cli::{DirArtifacts, DirArtifactsError};
use tempfile::TempDir;

/// An artifacts directory holding `assay-plan-x/run.json` with `bytes`.
fn artifacts(bytes: &[u8]) -> Result<TempDir, Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    fs::create_dir(root.path().join("assay-plan-x"))?;
    fs::write(root.path().join("assay-plan-x/run.json"), bytes)?;
    Ok(root)
}

fn store(root: &Path) -> DirArtifacts {
    DirArtifacts::new(root)
}

#[test]
fn a_file_under_or_at_the_limit_is_found_as_it_is() -> Result<(), Box<dyn Error>> {
    let root = artifacts(b"{\"a\":1}")?;
    let found = store(root.path()).fetch("assay-plan-x", "run.json", 100)?;
    assert_eq!(found, Fetched::Found(b"{\"a\":1}".to_vec()));
    let at_limit = store(root.path()).fetch("assay-plan-x", "run.json", 7)?;
    assert_eq!(at_limit, Fetched::Found(b"{\"a\":1}".to_vec()));
    Ok(())
}

#[test]
fn a_file_one_byte_over_the_limit_is_too_large() -> Result<(), Box<dyn Error>> {
    let root = artifacts(b"{\"a\":1}")?;
    assert_eq!(
        store(root.path()).fetch("assay-plan-x", "run.json", 6)?,
        Fetched::TooLarge
    );
    Ok(())
}

#[test]
fn an_absent_artifact_or_file_is_missing() -> Result<(), Box<dyn Error>> {
    let root = artifacts(b"{}")?;
    let store = store(root.path());
    assert_eq!(
        store.fetch("assay-plan-y", "run.json", 100)?,
        Fetched::Missing
    );
    assert_eq!(
        store.fetch("assay-plan-x", "temper.json", 100)?,
        Fetched::Missing
    );
    Ok(())
}

#[test]
fn a_symlink_is_refused_not_followed() -> Result<(), Box<dyn Error>> {
    let root = artifacts(b"{}")?;
    symlink(
        root.path().join("assay-plan-x/run.json"),
        root.path().join("assay-plan-x/link.json"),
    )?;
    symlink(
        root.path().join("assay-plan-x"),
        root.path().join("assay-plan-z"),
    )?;
    let store = store(root.path());
    assert_matches!(
        store.fetch("assay-plan-x", "link.json", 100),
        Err(DirArtifactsError::NotARegularFile { .. })
    );
    assert_matches!(
        store.fetch("assay-plan-z", "run.json", 100),
        Err(DirArtifactsError::NotARegularFile { .. })
    );
    Ok(())
}

#[test]
fn a_directory_is_refused() -> Result<(), Box<dyn Error>> {
    let root = artifacts(b"{}")?;
    fs::create_dir(root.path().join("assay-plan-x/sub.json"))?;
    assert_matches!(
        store(root.path()).fetch("assay-plan-x", "sub.json", 100),
        Err(DirArtifactsError::NotARegularFile { .. })
    );
    Ok(())
}

#[test]
fn a_name_that_is_not_one_plain_component_is_refused() -> Result<(), Box<dyn Error>> {
    let root = artifacts(b"{}")?;
    let store = store(root.path());
    for (artifact, file) in [
        ("..", "run.json"),
        ("assay-plan-x", ".."),
        ("assay-plan-x/..", "run.json"),
        ("assay-plan-x", "../assay-plan-x/run.json"),
        ("assay-plan-x", "sub/run.json"),
        (".", "run.json"),
        ("", "run.json"),
        ("assay-plan-x", ""),
        ("assay-plan-x", "a\\b"),
        ("assay-plan-x", "a\0b"),
    ] {
        assert_matches!(
            store.fetch(artifact, file, 100),
            Err(DirArtifactsError::Name),
            "{artifact:?}/{file:?}"
        );
    }
    Ok(())
}
