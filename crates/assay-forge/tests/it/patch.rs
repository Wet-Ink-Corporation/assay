//! One changed file's unified diff and its hunk headers: `ObjectRepo::file_patch`, read for the
//! local review's context pack and validation's hunks (02 §6, 04 §4 step 3).

use std::error::Error;
use std::fs;

use assay_forge::git::{FilePatch, GitError, GitOperation, HunkSpan, LineRange};

use crate::support::{fixture, fixture_with_head, fixture_with_removals, write};

const fn span(base: (u64, u64), head: (u64, u64)) -> HunkSpan {
    HunkSpan {
        base: LineRange {
            start: base.0,
            count: base.1,
        },
        head: LineRange {
            start: head.0,
            count: head.1,
        },
    }
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_new_file_is_one_hunk_of_head_lines() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("notes.txt", b"one\ntwo\nthree\n")])?;

    let patch = fixture
        .repo
        .file_patch(&fixture.base, &fixture.head, &["notes.txt"])?;

    assert_eq!(patch.hunks, [span((0, 0), (1, 3))]);
    assert!(
        patch
            .text
            .starts_with(b"diff --git a/notes.txt b/notes.txt\n"),
        "the text is git's unified diff"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_edit_has_one_hunk_with_its_context() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")])?;

    let patch = fixture
        .repo
        .file_patch(&fixture.base, &fixture.head, &["src/lib.rs"])?;

    assert_eq!(patch.hunks, [span((1, 3), (1, 3))]);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_rename_with_an_edit_is_one_file() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        write(
            work,
            "legacy/moved.rs",
            b"pub fn kept() {}\npub fn more() {}\n",
        )?;
        fs::remove_file(work.join("legacy/kept.rs"))?;
        Ok(())
    })?;

    let patch = fixture.repo.file_patch(
        &fixture.base,
        &fixture.head,
        &["legacy/kept.rs", "legacy/moved.rs"],
    )?;

    assert_eq!(patch.hunks, [span((1, 1), (1, 2))]);
    assert!(
        String::from_utf8_lossy(&patch.text).contains("rename from legacy/kept.rs\n"),
        "git detected the rename within the two paths"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_binary_file_has_no_hunks() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("logo.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR")])?;

    let patch = fixture
        .repo
        .file_patch(&fixture.base, &fixture.head, &["logo.png"])?;

    assert_eq!(patch.hunks, Vec::new(), "a binary diff has no hunk headers");
    assert!(
        String::from_utf8_lossy(&patch.text)
            .ends_with("Binary files /dev/null and b/logo.png differ\n"),
        "git says the file is binary, got {patch:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_deletion_is_one_hunk_of_base_lines() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_removals(&[], &["legacy/kept.rs"])?;

    let patch = fixture
        .repo
        .file_patch(&fixture.base, &fixture.head, &["legacy/kept.rs"])?;

    assert_eq!(patch.hunks, [span((1, 1), (0, 0))]);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_unchanged_path_has_an_empty_patch() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("notes.txt", b"one\n")])?;

    let patch = fixture
        .repo
        .file_patch(&fixture.base, &fixture.head, &["README.md"])?;

    assert_eq!(patch, FilePatch::default());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn two_unrelated_files_in_one_patch_are_refused() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n"),
        ("README.md", b"# changed\n"),
    ])?;

    let result =
        fixture
            .repo
            .file_patch(&fixture.base, &fixture.head, &["README.md", "src/lib.rs"]);

    assert!(
        matches!(
            result,
            Err(GitError::Malformed {
                operation: GitOperation::Diff,
                detail: "the patch holds more than one file",
            })
        ),
        "hunks of two files cannot be told apart, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_path_that_looks_like_a_pattern_names_only_its_own_file() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("*", b"star\n"),
        (":x", b"colon\n"),
        ("other.txt", b"other\n"),
    ])?;

    let star = fixture
        .repo
        .file_patch(&fixture.base, &fixture.head, &["*"])?;
    let colon = fixture
        .repo
        .file_patch(&fixture.base, &fixture.head, &[":x"])?;

    assert!(
        star.text.starts_with(b"diff --git a/* b/*\n"),
        "{}",
        String::from_utf8_lossy(&star.text)
    );
    assert_eq!(
        star.hunks,
        [span((0, 0), (1, 1))],
        "one file, not every file"
    );
    assert!(
        colon.text.starts_with(b"diff --git a/:x b/:x\n"),
        "{}",
        String::from_utf8_lossy(&colon.text)
    );
    Ok(())
}
