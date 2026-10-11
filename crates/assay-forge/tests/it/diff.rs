//! Measuring a PR with `git diff --numstat --text` under a wall-clock cap.

use std::error::Error;
use std::time::Duration;

use assay_forge::git::{CommitSha, DiffOutcome, GitError, GitOperation, LineCount};

use crate::support::fixture;

const CAP: Duration = Duration::from_secs(60);

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn measures_lines_added_and_deleted_per_file() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n// end\n")])?;

    let outcome = fixture.repo.numstat(&fixture.base, &fixture.head, CAP)?;

    let DiffOutcome::Measured(changes) = outcome else {
        panic!("a two-commit diff finishes well within a minute, got {outcome:?}");
    };
    let [change] = changes.as_slice() else {
        panic!("one file changed, got {changes:?}");
    };
    assert_eq!(
        (change.path(), change.added(), change.deleted()),
        ("src/lib.rs", LineCount::Counted(3), LineCount::Counted(2)),
        "two lines rewritten and one appended"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_file_git_will_not_measure_counts_as_maximum_size() -> Result<(), Box<dyn Error>> {
    // `--numstat` still prints `-` for content with NUL bytes even under `--text` (git 2.43).
    let fixture = fixture(&[("logo.bin", b"\0\x01\x02\nbinary\n")])?;

    let outcome = fixture.repo.numstat(&fixture.base, &fixture.head, CAP)?;

    let DiffOutcome::Measured(changes) = outcome else {
        panic!("a two-commit diff finishes well within a minute, got {outcome:?}");
    };
    let [change] = changes.as_slice() else {
        panic!("one file changed, got {changes:?}");
    };
    assert_eq!(
        (change.added().for_tiering(), change.deleted().for_tiering()),
        (u64::MAX, u64::MAX),
        "a residual `-` must force the largest tier, never count as zero"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_minus_diff_attribute_cannot_shrink_the_measured_size() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n"),
        (".gitattributes", b"* -diff\n"),
    ])?;

    let outcome = fixture.repo.numstat(&fixture.base, &fixture.head, CAP)?;

    let DiffOutcome::Measured(changes) = outcome else {
        panic!("a two-commit diff finishes well within a minute, got {outcome:?}");
    };
    let lib = changes.iter().find(|change| change.path() == "src/lib.rs");
    assert!(
        lib.is_some_and(|lib| lib.added().for_tiering() >= 2),
        "a PR must not hide its changed lines behind `* -diff`, got {changes:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_commit_missing_from_the_repository_fails_instead_of_measuring_nothing()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;
    let absent: CommitSha = "1111111111111111111111111111111111111111".parse()?;

    let result = fixture.repo.numstat(&fixture.base, &absent, CAP);

    assert!(
        matches!(
            result,
            Err(GitError::Failed {
                operation: GitOperation::Diff,
                ..
            })
        ),
        "an absent head must fail closed, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_diff_past_its_cap_needs_split_instead_of_hanging() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"changed\n")])?;

    let outcome = fixture
        .repo
        .numstat(&fixture.base, &fixture.head, Duration::ZERO)?;

    assert_eq!(
        outcome,
        DiffOutcome::NeedsSplit,
        "a diff that does not finish within its cap must classify the PR as needs-split"
    );
    Ok(())
}
