//! The PR's history for re-review (`docs/architecture/04-pipeline.md` §7, #99): how the last
//! completed run's head stands to this head, and `git range-diff` between them, over fixture
//! repositories with a push on top, a force-push and an unreachable previous head.

use std::error::Error;

use assay_forge::git::{
    Ancestry, CommitSha, GitError, GitOperation, RangeDiffCommits, RangeDiffOutcome,
};

use crate::support::{Forced, fixture, forced_fixture};

#[test]
fn a_previous_head_the_push_built_on_is_an_ancestor() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")])?;

    let ancestry = fixture.repo.ancestry(&fixture.base, &fixture.head)?;

    assert_eq!(ancestry, Ancestry::Ancestor);
    Ok(())
}

#[test]
fn the_head_itself_counts_as_an_ancestor() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")])?;

    let ancestry = fixture.repo.ancestry(&fixture.head, &fixture.head)?;

    assert_eq!(ancestry, Ancestry::Ancestor);
    Ok(())
}

#[test]
fn a_force_pushed_previous_head_is_rewritten() -> Result<(), Box<dyn Error>> {
    let forced = forced_fixture()?;

    let ancestry = forced
        .fixture
        .repo
        .ancestry(&forced.previous, &forced.fixture.head)?;

    assert_eq!(ancestry, Ancestry::Rewritten);
    Ok(())
}

#[test]
fn a_previous_head_missing_from_the_repository_is_unreachable() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")])?;
    let missing: CommitSha = "1111111111111111111111111111111111111111".parse()?;

    let ancestry = fixture.repo.ancestry(&missing, &fixture.head)?;

    assert_eq!(ancestry, Ancestry::Unreachable);
    Ok(())
}

#[test]
fn a_missing_head_is_an_error_not_an_answer() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")])?;
    let missing: CommitSha = "1111111111111111111111111111111111111111".parse()?;

    let ancestry = fixture.repo.ancestry(&fixture.base, &missing);

    assert!(
        matches!(
            ancestry,
            Err(GitError::Failed {
                operation: GitOperation::IsAncestor,
                ..
            })
        ),
        "{ancestry:?}"
    );
    Ok(())
}

/// The force-push's base, earlier head and head.
fn commits(forced: &Forced) -> RangeDiffCommits<'_> {
    RangeDiffCommits {
        base: &forced.fixture.base,
        previous: &forced.previous,
        head: &forced.fixture.head,
    }
}

#[test]
fn the_range_diff_of_a_force_push_pairs_the_old_and_new_commits() -> Result<(), Box<dyn Error>> {
    let forced = forced_fixture()?;
    let fixture = &forced.fixture;

    let outcome = fixture.repo.range_diff(commits(&forced), 64 * 1024)?;

    let RangeDiffOutcome::Listed(bytes) = outcome else {
        return Err("a small range-diff was refused".into());
    };
    let text = String::from_utf8(bytes)?;
    let headers: Vec<&str> = text
        .lines()
        .filter(|line| line.contains(':') && !line.starts_with(' '))
        .collect();
    let [old, new] = headers.as_slice() else {
        return Err(format!("expected two commit lines, got {text:?}").into());
    };
    assert!(old.ends_with(" previous"), "{old}");
    assert!(new.ends_with(" head"), "{new}");
    Ok(())
}

#[test]
fn a_range_diff_past_the_limit_is_too_large() -> Result<(), Box<dyn Error>> {
    let forced = forced_fixture()?;
    let fixture = &forced.fixture;

    let outcome = fixture.repo.range_diff(commits(&forced), 8)?;

    assert_eq!(outcome, RangeDiffOutcome::TooLarge);
    Ok(())
}
