//! The open check: what is read must be the file and directory that were checked, so a path
//! swapped between the check and the open is refused rather than followed (#347 F2).

use std::fs;

use assay_app::Fetched;

use super::{DirArtifactsError, Kind, read, read_in};

/// The checked identity of the regular file at `path`.
fn checked(path: &std::path::Path) -> super::Checked {
    match super::kind(path) {
        Ok(Kind::File(checked) | Kind::Directory(checked)) => checked,
        Ok(_) | Err(_) => panic!("{} is not a file or directory", path.display()),
    }
}

#[test]
fn the_file_opened_is_read_when_it_is_the_one_checked() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let path = root.path().join("run.json");
    fs::write(&path, b"{}").expect("written");

    let read = read(&path, checked(&path), 16);

    assert!(matches!(read, Ok(Fetched::Found(bytes)) if bytes == b"{}"));
}

#[test]
fn a_file_swapped_after_its_check_is_refused() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let (path, other) = (root.path().join("run.json"), root.path().join("other.json"));
    fs::write(&path, b"{}").expect("written");
    fs::write(&other, b"[]").expect("written");
    let before = checked(&other);

    // As if `run.json` had been replaced by another file between the check and the open.
    let read = read(&path, before, 16);

    assert!(
        matches!(&read, Err(DirArtifactsError::NotARegularFile { path: refused }) if *refused == path),
        "{read:?}"
    );
}

#[test]
fn a_directory_replaced_while_its_file_is_read_is_refused() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let artifact = root.path().join("assay-plan-1");
    fs::create_dir(&artifact).expect("created");
    fs::write(artifact.join("run.json"), b"{}").expect("written");
    let swapped = root.path().join("assay-plan-2");
    fs::create_dir(&swapped).expect("created");
    let before = checked(&swapped);

    let read = super::read_checked_in(&artifact, before, "run.json", 16);

    assert!(
        matches!(&read, Err(DirArtifactsError::NotARegularFile { path }) if *path == artifact),
        "{read:?}"
    );
    assert!(matches!(
        read_in(&artifact, "run.json", 16),
        Ok(Fetched::Found(_))
    ));
}
