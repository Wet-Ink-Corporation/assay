//! Reading a commit's tree as data: `ObjectRepo::list_tree`, `read_blobs` and `hash_blob`
//! (03 §2.1: the temper adapter's Z2s checks read base and head this way), each bounded by
//! `TreeLimits`.

use std::error::Error;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use assay_forge::git::{BlobRef, EntryKind, GitError, TreeEntry, TreeLimits};

use crate::support::{fixture, fixture_with_head, write};

const GENEROUS: TreeLimits = TreeLimits {
    max_entries: 1000,
    max_total_bytes: 1 << 20,
};

/// Each entry as its path and kind, for whole-value comparisons.
fn kinds(entries: &[TreeEntry]) -> Vec<(&str, EntryKind)> {
    entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.kind))
        .collect()
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn lists_every_file_under_a_directory_and_nothing_beside_it() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        (".claude/temper/temper.mjs", b"main\n"),
        (".claude/temper/lib/scan.mjs", b"scan\n"),
        (".claude/temper-baseline.json", b"{}\n"),
        (".claude/temper.json", b"{}\n"),
    ])?;

    let entries = fixture
        .repo
        .list_tree(&fixture.head, Some(".claude/temper"), GENEROUS)?;

    assert_eq!(
        kinds(&entries),
        [
            (".claude/temper/lib/scan.mjs", EntryKind::File),
            (".claude/temper/temper.mjs", EntryKind::File),
        ],
        "a directory is listed recursively, and a sibling sharing its prefix is not under it"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn lists_the_whole_tree_without_a_path() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;

    let entries = fixture.repo.list_tree(&fixture.base, None, GENEROUS)?;

    assert_eq!(
        kinds(&entries),
        [
            ("Cargo.toml", EntryKind::File),
            ("README.md", EntryKind::File),
            ("legacy/kept.rs", EntryKind::File),
            ("src/lib.rs", EntryKind::File),
        ],
        "every file of the commit"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_absent_path_has_no_entries() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[(".claude/temper.json", b"{}\n")])?;

    let entries = fixture
        .repo
        .list_tree(&fixture.base, Some(".claude/temper.json"), GENEROUS)?;

    assert_eq!(kinds(&entries), [], "the file exists only at head");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn symlinks_and_executables_are_told_apart_from_plain_files() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        write(work, "tools/run.sh", b"#!/bin/sh\n")?;
        let script = work.join("tools/run.sh");
        let mut permissions = fs::metadata(&script)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions)?;
        symlink("../README.md", work.join("tools/readme"))?;
        Ok(())
    })?;

    let entries = fixture
        .repo
        .list_tree(&fixture.head, Some("tools"), GENEROUS)?;

    assert_eq!(
        kinds(&entries),
        [
            ("tools/readme", EntryKind::Symlink),
            ("tools/run.sh", EntryKind::Executable),
        ],
        "each mode is reported, and a symlink is never followed"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_listing_over_the_entry_limit_fails_closed() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;
    let limits = TreeLimits {
        max_entries: 3,
        ..GENEROUS
    };

    let result = fixture.repo.list_tree(&fixture.base, None, limits);

    assert!(
        matches!(result, Err(GitError::TooManyEntries { max: 3, found: 4 })),
        "four files against a limit of three, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn reads_the_bytes_of_listed_blobs_in_one_go() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n")])?;
    let base = fixture
        .repo
        .list_tree(&fixture.base, Some("src"), GENEROUS)?;
    let head = fixture
        .repo
        .list_tree(&fixture.head, Some("src"), GENEROUS)?;
    let entries: Vec<BlobRef<'_>> = base.iter().chain(&head).map(BlobRef::from).collect();

    let blobs = fixture.repo.read_blobs(&entries, GENEROUS)?;

    assert_eq!(
        blobs,
        [
            b"pub fn one() -> u8 {\n    1\n}\n".to_vec(),
            b"pub fn two() -> u8 {\n    2\n}\n".to_vec(),
        ],
        "each listed blob, in the order asked"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn blobs_over_the_byte_limit_are_refused_before_any_read() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;
    let entries = fixture.repo.list_tree(&fixture.base, None, GENEROUS)?;
    let all: Vec<BlobRef<'_>> = entries.iter().map(BlobRef::from).collect();
    let total: u64 = entries.iter().filter_map(|entry| entry.size).sum();
    let limits = TreeLimits {
        max_total_bytes: total - 1,
        ..GENEROUS
    };

    let result = fixture.repo.read_blobs(&all, limits);

    assert!(
        matches!(
            result,
            Err(GitError::TooManyBytes { max, found }) if max == total - 1 && found == total
        ),
        "the listed sizes decide, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn hashes_bytes_to_the_object_id_git_lists_for_them() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;
    let listed = fixture
        .repo
        .list_tree(&fixture.base, Some("README.md"), GENEROUS)?;

    let id = fixture.repo.hash_blob(b"# fixture\n")?;

    assert_eq!(
        listed
            .iter()
            .map(|entry| entry.object.as_str())
            .collect::<Vec<_>>(),
        [id.as_str()],
        "the same bytes have the same blob id, without reading the blob"
    );
    Ok(())
}
