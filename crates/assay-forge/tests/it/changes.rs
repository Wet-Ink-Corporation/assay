//! The PR's file list, `git diff --raw` and `--numstat` over `base...head`, for `plan`
//! (`docs/architecture/04-pipeline.md` §2).

use std::error::Error;
use std::time::Duration;

use assay_forge::git::{ChangeListing, ChangedFile, EntryKind, LineCount};

use crate::support::{Fixture, fixture, fixture_with_head, fixture_with_removals, forked_fixture};

const CAP: Duration = Duration::from_secs(60);

fn listed(fixture: &Fixture) -> Result<Vec<ChangedFile>, Box<dyn Error>> {
    match fixture
        .repo
        .changed_files(&fixture.base, &fixture.head, CAP)?
    {
        ChangeListing::Listed(files) => Ok(files),
        ChangeListing::NeedsSplit => Err("a fixture diff finishes well within a minute".into()),
    }
}

/// (path, previous path, base kind, head kind, added, deleted) of each file.
type Row<'a> = (
    &'a str,
    Option<&'a str>,
    Option<EntryKind>,
    Option<EntryKind>,
    LineCount,
    LineCount,
);

fn rows(files: &[ChangedFile]) -> Vec<Row<'_>> {
    files
        .iter()
        .map(|file| {
            (
                file.path(),
                file.previous_path(),
                file.base_kind(),
                file.head_kind(),
                file.added(),
                file.deleted(),
            )
        })
        .collect()
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn lists_only_the_heads_changes_against_the_merge_base() -> Result<(), Box<dyn Error>> {
    // The base branch moved on after the fork: its commit is not the PR's to review.
    let fixture = forked_fixture(
        &[("base-only.txt", b"landed on main after the fork\n")],
        &[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")],
    )?;

    let files = listed(&fixture)?;

    assert_eq!(
        rows(&files),
        [(
            "src/lib.rs",
            None,
            Some(EntryKind::File),
            Some(EntryKind::File),
            LineCount::Counted(1),
            LineCount::Counted(1),
        )],
        "base...head diffs from the merge base, never against the base's own changes"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn reports_a_rename_with_both_paths() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_removals(
        &[("moved/kept.rs", b"pub fn kept() {}\n")],
        &["legacy/kept.rs"],
    )?;

    let files = listed(&fixture)?;

    assert_eq!(
        rows(&files),
        [(
            "moved/kept.rs",
            Some("legacy/kept.rs"),
            Some(EntryKind::File),
            Some(EntryKind::File),
            LineCount::Counted(0),
            LineCount::Counted(0),
        )],
        "a rename is one record with its old and new path"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn reports_additions_and_deletions_with_one_side_absent() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_removals(&[("docs/new.md", b"new\n")], &["README.md"])?;

    let files = listed(&fixture)?;

    assert_eq!(
        rows(&files),
        [
            (
                "README.md",
                None,
                Some(EntryKind::File),
                None,
                LineCount::Counted(0),
                LineCount::Counted(1),
            ),
            (
                "docs/new.md",
                None,
                None,
                Some(EntryKind::File),
                LineCount::Counted(1),
                LineCount::Counted(0),
            ),
        ],
        "a deletion has no head side and an addition no base side"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn reports_a_symlink_by_its_mode() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        std::os::unix::fs::symlink("src/lib.rs", work.join("lib-link.rs"))?;
        Ok(())
    })?;

    let files = listed(&fixture)?;

    assert_eq!(
        files
            .iter()
            .map(|file| (file.path(), file.base_kind(), file.head_kind()))
            .collect::<Vec<_>>(),
        [("lib-link.rs", None, Some(EntryKind::Symlink))],
        "tiering keeps a symlink out of docs and exclusions, so its mode must survive"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn lists_a_file_turned_into_a_symlink_once_with_both_kinds() -> Result<(), Box<dyn Error>> {
    // A typechange (`T`): the path keeps its name but changes mode.
    let fixture = fixture_with_head(|work| {
        std::fs::remove_file(work.join("README.md"))?;
        std::os::unix::fs::symlink("src/lib.rs", work.join("README.md"))?;
        Ok(())
    })?;

    let files = listed(&fixture)?;

    assert_eq!(
        files
            .iter()
            .map(|file| (
                file.path(),
                file.previous_path(),
                file.base_kind(),
                file.head_kind()
            ))
            .collect::<Vec<_>>(),
        [(
            "README.md",
            None,
            Some(EntryKind::File),
            Some(EntryKind::Symlink)
        )],
        "a typechange is one record carrying both sides' modes"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn reports_a_submodule_by_its_mode() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        // An unpopulated submodule's directory, so `git add --all` keeps the gitlink.
        std::fs::create_dir_all(work.join("vendor/sub"))?;
        crate::support::plain_git(
            work,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0123456789abcdef0123456789abcdef01234567,vendor/sub",
            ],
        )?;
        Ok(())
    })?;

    let files = listed(&fixture)?;

    assert_eq!(
        files
            .iter()
            .map(|file| (file.path(), file.base_kind(), file.head_kind()))
            .collect::<Vec<_>>(),
        [("vendor/sub", None, Some(EntryKind::Submodule))],
        "a gitlink is never docs and never excluded, so its mode must survive"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_file_git_will_not_measure_is_unmeasured() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("logo.bin", b"\0\x01\x02\nbinary\n")])?;

    let files = listed(&fixture)?;

    assert_eq!(
        rows(&files),
        [(
            "logo.bin",
            None,
            None,
            Some(EntryKind::File),
            LineCount::Unmeasured,
            LineCount::Unmeasured,
        )],
        "a residual `-` must force the largest tier, never count as zero"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_listing_past_its_cap_needs_split_instead_of_hanging() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"changed\n")])?;

    let listing = fixture
        .repo
        .changed_files(&fixture.base, &fixture.head, Duration::ZERO)?;

    assert_eq!(
        listing,
        ChangeListing::NeedsSplit,
        "a diff that does not finish within its cap is no file list at all"
    );
    Ok(())
}
