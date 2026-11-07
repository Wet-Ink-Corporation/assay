//! [`ObjectRepo::reading_checkout`] (#372): a checkout of a fetched commit that a pinned tool
//! (the Weigh-In gate's `weighin ci-check`) reads as a repository: head's files, written by the
//! hardened git with every symlink as a plain file, `HEAD` detached at the commit, and the
//! object-only repository's history (the base included) reachable through its alternates.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use review_forge::git::{GitError, TreeLimits};

use crate::support::{fixture_with_head, plain_git, write};

/// Every file and symlink under `dir` except `.git`, relative, with whether it is a regular
/// file.
fn written(dir: &Path) -> Result<Vec<(String, bool)>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next)? {
            let path = entry?.path();
            let kind = fs::symlink_metadata(&path)?.file_type();
            if path == dir.join(".git") {
                continue;
            }
            if kind.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(dir)?.to_string_lossy().into_owned();
                found.push((relative, kind.is_file()));
            }
        }
    }
    found.sort();
    Ok(found)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn writes_the_commits_files_with_symlinks_as_plain_files() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        write(work, ".kb/_ledger/journal/s1.jsonl", b"{}\n")?;
        std::os::unix::fs::symlink("/etc/hostname", work.join("host"))?;
        Ok(())
    })?;
    let scratch = tempfile::tempdir()?;
    let dest = scratch.path().join("tree");

    fixture
        .repo
        .reading_checkout(&fixture.head, &dest, TreeLimits::default())?;

    assert_eq!(
        written(&dest)?,
        [
            (".kb/_ledger/journal/s1.jsonl".to_owned(), true),
            ("Cargo.toml".to_owned(), true),
            ("README.md".to_owned(), true),
            ("host".to_owned(), true),
            ("legacy/kept.rs".to_owned(), true),
            ("src/lib.rs".to_owned(), true),
        ],
        "every file of the commit, a symlink written as a plain file"
    );
    assert_eq!(fs::read_to_string(dest.join("host"))?, "/etc/hostname");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn head_is_detached_at_the_commit_and_the_base_is_reachable() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| write(work, "src/lib.rs", b"pub fn two() {}\n"))?;
    let scratch = tempfile::tempdir()?;
    let dest = fs::canonicalize(scratch.path())?.join("tree");

    fixture
        .repo
        .reading_checkout(&fixture.head, &dest, TreeLimits::default())?;

    let head = plain_git(&dest, &["rev-parse", "HEAD"])?;
    let top = plain_git(&dest, &["rev-parse", "--show-toplevel"])?;
    let branch = plain_git(&dest, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let range = format!("{}..HEAD", fixture.base);
    let commits = plain_git(&dest, &["rev-list", "--count", &range])?;
    assert_eq!(head.trim(), fixture.head.as_str());
    assert_eq!(
        Path::new(top.trim()),
        dest,
        "a non-bare repository rooted at dest"
    );
    assert_eq!(branch.trim(), "HEAD", "detached");
    assert_eq!(commits.trim(), "1", "the base's history is there");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn refuses_a_commit_past_the_entry_or_byte_limit_writing_nothing() -> Result<(), Box<dyn Error>> {
    // The fixture's head holds four files of 10, 27, 29 and 17 bytes: 83 in all.
    let fixture = fixture_with_head(|_| Ok(()))?;
    let scratch = tempfile::tempdir()?;
    let entries = TreeLimits {
        max_entries: 3,
        ..TreeLimits::default()
    };
    let bytes = TreeLimits {
        max_total_bytes: 82,
        ..TreeLimits::default()
    };
    let at_limit = TreeLimits {
        max_entries: 4,
        max_total_bytes: 83,
    };

    let refused = [entries, bytes].map(|limits| {
        let dest = scratch.path().join(format!("tree-{}", limits.max_entries));
        let result = fixture.repo.reading_checkout(&fixture.head, &dest, limits);
        (result, dest.exists())
    });
    let written =
        fixture
            .repo
            .reading_checkout(&fixture.head, &scratch.path().join("fits"), at_limit);

    let [(entries, entries_dest), (bytes, bytes_dest)] = refused;
    assert!(
        matches!(entries, Err(GitError::TooManyEntries { max: 3, found: 4 })),
        "{entries:?}"
    );
    assert!(
        matches!(bytes, Err(GitError::TooManyBytes { max: 82, found: 83 })),
        "{bytes:?}"
    );
    assert!(!entries_dest && !bytes_dest, "nothing written past a limit");
    assert!(
        written.is_ok(),
        "a commit at the limits is written: {written:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn refuses_a_destination_with_contents() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|_| Ok(()))?;
    let scratch = tempfile::tempdir()?;
    let dest = scratch.path().join("tree");
    write(&dest, "stale.txt", b"left over\n")?;

    let result = fixture
        .repo
        .reading_checkout(&fixture.head, &dest, TreeLimits::default());

    assert!(
        matches!(&result, Err(GitError::DirectoryNotEmpty { git_dir }) if *git_dir == dest),
        "{result:?}"
    );
    Ok(())
}
