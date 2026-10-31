//! The PR's history for re-review (`docs/architecture/04-pipeline.md` §7, #99): how the last
//! completed run's head stands to this head, and `git range-diff` between them, over fixture
//! repositories with a push on top, a force-push and an unreachable previous head.

use std::error::Error;
use std::time::Duration;

use assay_forge::git::{
    Ancestry, CommitSha, GitEnvironment, GitError, GitLimits, GitOperation, MAX_RANGE_DIFF_COMMITS,
    ObjectRepo, RANGE_DIFF_TIMEOUT, RangeDiffCommits, RangeDiffOutcome,
};

use crate::support::{
    Fixture, Forced, commit, fixture, forced_fixture, forked_fixture, long_fixture, plain_git,
    write,
};

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

#[test]
fn the_merge_base_of_a_forked_pr_is_its_fork_point() -> Result<(), Box<dyn Error>> {
    let fixture = forked_fixture(
        &[("docs/landed.md", b"on main after the fork\n")],
        &[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")],
    )?;
    // The base commit sits directly on the fork point.
    let fork: CommitSha = plain_git(
        &fixture.git_dir(),
        &["rev-parse", &format!("{}^", fixture.base)],
    )?
    .trim()
    .parse()?;

    let merge_base = fixture.repo.merge_base(&fixture.base, &fixture.head)?;

    assert_eq!(merge_base, Some(fork));
    Ok(())
}

#[test]
fn commits_without_common_history_have_no_merge_base() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    write(&work, "README.md", b"# one\n")?;
    let one = commit(&work, "one")?;
    plain_git(&work, &["checkout", "--quiet", "--orphan", "other"])?;
    write(&work, "README.md", b"# other\n")?;
    let other = commit(&work, "other")?;
    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    let one_ref = format!("{one}:refs/heads/one");
    plain_git(
        &work,
        &[
            "push",
            "--quiet",
            destination,
            &one_ref,
            "other:refs/heads/other",
        ],
    )?;

    let merge_base = repo.merge_base(&one, &other)?;

    assert_eq!(merge_base, None);
    Ok(())
}

#[test]
fn a_merge_base_with_a_missing_commit_is_an_error_not_an_answer() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")])?;
    let missing: CommitSha = "1111111111111111111111111111111111111111".parse()?;

    let merge_base = fixture.repo.merge_base(&fixture.base, &missing);

    assert!(
        matches!(
            merge_base,
            Err(GitError::Failed {
                operation: GitOperation::MergeBase,
                ..
            })
        ),
        "{merge_base:?}"
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

    let outcome = fixture
        .repo
        .range_diff(commits(&forced), 64 * 1024, RANGE_DIFF_TIMEOUT)?;

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

    let outcome = fixture
        .repo
        .range_diff(commits(&forced), 8, RANGE_DIFF_TIMEOUT)?;

    assert_eq!(outcome, RangeDiffOutcome::TooLarge);
    Ok(())
}

/// Review F2 (H-13): range-diff runs within its own cap, and running past it is not an error.
#[test]
fn a_range_diff_past_its_timeout_is_too_slow() -> Result<(), Box<dyn Error>> {
    let forced = forced_fixture()?;

    let outcome = forced
        .fixture
        .repo
        .range_diff(commits(&forced), 64 * 1024, Duration::ZERO)?;

    assert_eq!(outcome, RangeDiffOutcome::TooSlow);
    Ok(())
}

/// Review F4: only a missing commit is unreachable; a repository that cannot be read is an error.
#[test]
fn a_broken_object_store_is_an_error_not_unreachable() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")])?;
    std::fs::remove_dir_all(fixture.git_dir().join("objects"))?;

    let ancestry = fixture.repo.ancestry(&fixture.base, &fixture.head);

    assert!(
        matches!(
            ancestry,
            Err(GitError::Failed {
                operation: GitOperation::RevParse,
                ..
            })
        ),
        "{ancestry:?}"
    );
    Ok(())
}

/// Review F5: the limit admits output of exactly its length.
#[test]
fn a_range_diff_at_its_output_length_is_listed_and_one_byte_less_is_too_large()
-> Result<(), Box<dyn Error>> {
    let forced = forced_fixture()?;
    let repo = &forced.fixture.repo;
    let RangeDiffOutcome::Listed(bytes) =
        repo.range_diff(commits(&forced), 64 * 1024, RANGE_DIFF_TIMEOUT)?
    else {
        return Err("a small range-diff was refused".into());
    };
    let length = bytes.len();

    let at = repo.range_diff(commits(&forced), length, RANGE_DIFF_TIMEOUT)?;
    let under = repo.range_diff(commits(&forced), length - 1, RANGE_DIFF_TIMEOUT)?;

    assert_eq!(at, RangeDiffOutcome::Listed(bytes));
    assert_eq!(under, RangeDiffOutcome::TooLarge);
    Ok(())
}

#[test]
fn a_range_diff_from_a_missing_base_is_an_error() -> Result<(), Box<dyn Error>> {
    let forced = forced_fixture()?;
    let missing: CommitSha = "1111111111111111111111111111111111111111".parse()?;

    let outcome = forced.fixture.repo.range_diff(
        RangeDiffCommits {
            base: &missing,
            ..commits(&forced)
        },
        64 * 1024,
        RANGE_DIFF_TIMEOUT,
    );

    assert!(
        matches!(
            outcome,
            Err(GitError::Failed {
                operation: GitOperation::RevListCount,
                ..
            })
        ),
        "{outcome:?}"
    );
    Ok(())
}

/// Review F2 (H-13): each side's commits are counted first, and past the cap range-diff does
/// not run.
#[test]
fn a_side_at_the_commit_cap_is_range_diffed_and_one_more_is_too_large() -> Result<(), Box<dyn Error>>
{
    let at_cap = long_fixture(MAX_RANGE_DIFF_COMMITS)?;
    let over = long_fixture(MAX_RANGE_DIFF_COMMITS + 1)?;

    let listed = at_cap
        .repo
        .range_diff(from_base(&at_cap), 1 << 20, RANGE_DIFF_TIMEOUT)?;
    let refused = over
        .repo
        .range_diff(from_base(&over), 1 << 20, RANGE_DIFF_TIMEOUT)?;

    assert!(matches!(listed, RangeDiffOutcome::Listed(_)), "{listed:?}");
    assert_eq!(refused, RangeDiffOutcome::TooLarge);
    Ok(())
}

/// A range-diff of everything the fixture's head adds, against nothing added before.
fn from_base(fixture: &Fixture) -> RangeDiffCommits<'_> {
    RangeDiffCommits {
        base: &fixture.base,
        previous: &fixture.base,
        head: &fixture.head,
    }
}
