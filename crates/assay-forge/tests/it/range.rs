//! The PR range for the eligibility rule (`docs/architecture/04-pipeline.md` §5, R3-07,
//! R3-22): `rev-list --parents base..head` under a cap, a commit's tree, and
//! `merge-tree --write-tree` for "Update branch" merges, over fixture repositories.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

use assay_forge::git::{
    CommitSha, GitEnvironment, GitLimits, MergeOutcome, ObjectRepo, RangeCommits,
};
use tempfile::TempDir;

use crate::support::{commit, long_fixture, plain_git, write};

type Outcome = Result<(), Box<dyn Error>>;

/// A PR commit on a base that moved on, merged from the base cleanly and with a conflict.
struct Merges {
    _scratch: TempDir,
    repo: ObjectRepo,
    base: CommitSha,
    first: CommitSha,
    tip: CommitSha,
    clean: CommitSha,
    conflicting: CommitSha,
}

fn merges() -> Result<Merges, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(
        scratch.path(),
        &["init", "--quiet", "--initial-branch=main", "work"],
    )?;
    write(&work, "README.md", b"# fixture\n")?;
    write(&work, "src/lib.rs", b"pub fn one() -> u8 {\n    1\n}\n")?;
    let base = commit(&work, "base")?;
    plain_git(&work, &["checkout", "--quiet", "-b", "pr"])?;
    write(&work, "src/pr.rs", b"pub fn pr() {}\n")?;
    let first = commit(&work, "first")?;
    plain_git(&work, &["checkout", "--quiet", "main"])?;
    write(&work, "docs.md", b"docs\n")?;
    let tip = commit(&work, "tip")?;
    plain_git(&work, &["checkout", "--quiet", "pr"])?;
    plain_git(&work, &["merge", "--quiet", "--no-ff", "--no-edit", "main"])?;
    let clean: CommitSha = plain_git(&work, &["rev-parse", "HEAD"])?.trim().parse()?;
    plain_git(
        &work,
        &["checkout", "--quiet", "-b", "clash", first.as_str()],
    )?;
    write(&work, "docs.md", b"other docs\n")?;
    commit(&work, "clash")?;
    let conflicting_parent: String = plain_git(&work, &["rev-parse", "HEAD"])?;
    plain_git(&work, &["checkout", "--quiet", "main"])?;
    plain_git(&work, &["checkout", "--quiet", conflicting_parent.trim()])?;
    let attempt = plain_git(&work, &["merge", "--quiet", "--no-edit", "main"]);
    if attempt.is_ok() {
        return Err("the fixture's second merge was meant to conflict".into());
    }
    write(&work, "docs.md", b"resolved\n")?;
    let conflicting = commit(&work, "resolved")?;
    plain_git(&work, &["branch", "resolved"])?;

    Ok(Merges {
        repo: import(scratch.path(), &work)?,
        _scratch: scratch,
        base,
        first,
        tip,
        clean,
        conflicting,
    })
}

/// An object-only repository under `scratch` holding `work`'s three branches.
fn import(scratch: &Path, work: &Path) -> Result<ObjectRepo, Box<dyn Error>> {
    let git_dir = scratch.join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git(
        work,
        &[
            "push",
            "--quiet",
            destination,
            "pr:refs/heads/pr",
            "main:refs/heads/main",
            "resolved:refs/heads/resolved",
        ],
    )?;
    Ok(repo)
}

#[test]
fn the_range_lists_each_commit_with_its_parents_newest_first() -> Outcome {
    let fixture = merges()?;

    let listed = fixture
        .repo
        .range_commits(&fixture.base, &fixture.clean, 250)?;

    let RangeCommits::Listed(commits) = listed else {
        return Err("the range fits".into());
    };
    let shapes: BTreeMap<&str, Vec<&str>> = commits
        .iter()
        .map(|commit| {
            let parents = commit.parents.iter().map(CommitSha::as_str).collect();
            (commit.commit.as_str(), parents)
        })
        .collect();
    let expected: BTreeMap<&str, Vec<&str>> = [
        (
            fixture.clean.as_str(),
            vec![fixture.first.as_str(), fixture.tip.as_str()],
        ),
        (fixture.tip.as_str(), vec![fixture.base.as_str()]),
        (fixture.first.as_str(), vec![fixture.base.as_str()]),
    ]
    .into_iter()
    .collect();
    assert_eq!(shapes, expected);
    assert_eq!(
        commits.first().map(|commit| &commit.commit),
        Some(&fixture.clean),
        "the head comes first"
    );
    Ok(())
}

#[test]
fn a_range_past_its_cap_is_too_many() -> Outcome {
    let fixture = long_fixture(251)?;

    let at_cap = fixture
        .repo
        .range_commits(&fixture.base, &fixture.head, 251)?;
    let past_cap = fixture
        .repo
        .range_commits(&fixture.base, &fixture.head, 250)?;

    assert!(
        matches!(&at_cap, RangeCommits::Listed(commits) if commits.len() == 251),
        "{at_cap:?}"
    );
    assert_eq!(past_cap, RangeCommits::TooMany);
    Ok(())
}

#[test]
fn a_clean_merge_writes_the_tree_of_the_merge_commit() -> Outcome {
    let fixture = merges()?;

    let merged = fixture.repo.merge_tree(&fixture.first, &fixture.tip)?;
    let tree = fixture.repo.tree_of(&fixture.clean)?;

    assert_eq!(merged, MergeOutcome::Clean(tree));
    Ok(())
}

#[test]
fn a_merge_that_conflicts_is_conflicted() -> Outcome {
    let fixture = merges()?;
    let parents = match fixture
        .repo
        .range_commits(&fixture.base, &fixture.conflicting, 250)?
    {
        RangeCommits::Listed(commits) => commits
            .into_iter()
            .find(|commit| commit.commit == fixture.conflicting)
            .map(|commit| commit.parents)
            .ok_or("the resolved merge is in its range")?,
        RangeCommits::TooMany => return Err("the range fits".into()),
    };
    let [ours, theirs] = parents.as_slice() else {
        return Err("the resolved merge has two parents".into());
    };

    let merged = fixture.repo.merge_tree(ours, theirs)?;

    assert_eq!(merged, MergeOutcome::Conflicted);
    Ok(())
}
