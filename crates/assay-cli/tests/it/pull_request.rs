//! `ForgePullRequest` as local mode's `ChangePatches` (#219 R2): one file's diff past forge's
//! output cap is "not shown", like a text over the patch budget, instead of failing the check.
//! And as the eligibility snapshot's `RangeObjects` (#58): an "Update branch" merge's parents,
//! tree and conflict-free merge, read from git objects. And as admit's `PriorHistory` (#60
//! ST-11): how the previous completed run's head stands to head, and the range-diff between them.

use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use assay_app::{
    Ancestry, ChangePatches, CommitId, FilePatch, MergeTree, PatchBudget, PriorHistory,
    RangeCommit, RangeDiffCommits, RangeDiffListing, RangeListing, RangeObjects,
};
use assay_cli::ForgePullRequest;
use assay_domain::{FileChange, FileKind, Lines};
use assay_forge::git::{GitEnvironment, GitLimits, ObjectRepo, TreeLimits};
use tempfile::TempDir;

/// The fixture change: `big.txt` gains many lines, `small.txt` one.
struct Change {
    scratch: TempDir,
    base: CommitId,
    head: CommitId,
}

impl Change {
    fn new() -> Result<Self, Box<dyn Error>> {
        let scratch = tempfile::tempdir()?;
        let work = scratch.path().join("work");
        fs::create_dir(&work)?;
        plain_git(&work, &["init", "--quiet"])?;
        fs::write(work.join("big.txt"), "start\n")?;
        fs::write(work.join("small.txt"), "start\n")?;
        let base = commit(&work, "base")?;
        fs::write(
            work.join("big.txt"),
            format!("start\n{}", "line\n".repeat(200)),
        )?;
        fs::write(work.join("small.txt"), "start\nend\n")?;
        let head = commit(&work, "head")?;
        Ok(Self {
            scratch,
            base,
            head,
        })
    }

    /// The patches of both files, read through an object repository whose git output is
    /// capped at `max_output_bytes`.
    fn patches(
        &self,
        name: &str,
        max_output_bytes: usize,
    ) -> Result<Vec<FilePatch>, Box<dyn Error>> {
        let git_dir = self.scratch.path().join(name);
        let limits = GitLimits {
            max_output_bytes,
            ..GitLimits::default()
        };
        let repo = ObjectRepo::init(&git_dir, GitEnvironment::from_process_env()?, limits)?;
        let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
        plain_git(
            &self.scratch.path().join("work"),
            &["push", "--quiet", destination, "HEAD:refs/heads/head"],
        )?;
        let objects = ForgePullRequest::new(&repo, TreeLimits::default(), Duration::from_secs(60));
        let files = [changed("big.txt"), changed("small.txt")];
        Ok(objects.patches(
            (&self.base, &self.head),
            &files,
            PatchBudget::new(usize::MAX),
        )?)
    }
}

fn changed(path: &str) -> FileChange {
    FileChange {
        path: path.to_owned(),
        renamed_from: None,
        lines: Lines::Counted {
            added: 1,
            removed: 0,
        },
        kind: FileKind::File,
    }
}

fn commit(work: &Path, message: &str) -> Result<CommitId, Box<dyn Error>> {
    plain_git(work, &["add", "--all"])?;
    plain_git(work, &["commit", "--quiet", "-m", message])?;
    Ok(plain_git(work, &["rev-parse", "HEAD"])?.trim().parse()?)
}

/// Runs git as a developer would, isolated from the machine's config. Test setup only.
fn plain_git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let mut git = Command::new("git");
    git.env_clear();
    if let Some(path) = env::var_os("PATH") {
        git.env("PATH", path);
    }
    let output = git
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// The byte length of `big.txt`'s patch, read with forge's default cap.
fn big_patch_len(change: &Change) -> Result<usize, Box<dyn Error>> {
    let patches = change.patches("measure.git", GitLimits::default().max_output_bytes)?;
    let text = patches
        .first()
        .and_then(|patch| patch.text.as_ref())
        .ok_or("big.txt's patch is shown under the default cap")?;
    Ok(text.len())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_patch_at_the_output_cap_is_shown() -> Result<(), Box<dyn Error>> {
    let change = Change::new()?;
    let len = big_patch_len(&change)?;

    let patches = change.patches("at-cap.git", len)?;

    let big = patches.first().ok_or("no patch for big.txt")?;
    assert_eq!(big.text.as_ref().map(String::len), Some(len));
    assert_eq!(big.hunks.len(), 1, "{big:?}");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_patch_one_byte_over_the_output_cap_is_not_shown_and_the_rest_are() -> Result<(), Box<dyn Error>>
{
    let change = Change::new()?;
    let len = big_patch_len(&change)?;

    let patches = change.patches("over-cap.git", len - 1)?;

    let [big, small] = patches.as_slice() else {
        return Err(format!("expected two patches, got {patches:?}").into());
    };
    assert_eq!(
        *big,
        FilePatch {
            text: None,
            hunks: vec![],
        },
        "a diff git could not print within the cap is not shown"
    );
    assert!(small.text.is_some(), "{small:?}");
    assert_eq!(small.hunks.len(), 1, "{small:?}");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_update_branch_merge_reads_as_a_clean_merge_of_its_parents() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    fs::create_dir(&work)?;
    plain_git(&work, &["init", "--quiet", "--initial-branch=main"])?;
    fs::write(work.join("lib.rs"), "fn one() {}\n")?;
    let base = commit(&work, "base")?;
    plain_git(&work, &["checkout", "--quiet", "-b", "pr"])?;
    fs::write(work.join("pr.rs"), "fn pr() {}\n")?;
    let first = commit(&work, "first")?;
    plain_git(&work, &["checkout", "--quiet", "main"])?;
    fs::write(work.join("docs.md"), "docs\n")?;
    let tip = commit(&work, "tip")?;
    plain_git(&work, &["checkout", "--quiet", "pr"])?;
    plain_git(&work, &["merge", "--quiet", "--no-ff", "--no-edit", "main"])?;
    let merge: CommitId = plain_git(&work, &["rev-parse", "HEAD"])?.trim().parse()?;
    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git(&work, &["push", "--quiet", destination, "pr:refs/heads/pr"])?;
    let objects = ForgePullRequest::new(&repo, TreeLimits::default(), Duration::from_secs(60));

    let listed = objects.range(&base, &merge, 250)?;
    let merged = objects.merge_tree(&first, &tip)?;

    let RangeListing::Listed(commits) = listed else {
        return Err("three commits fit the cap".into());
    };
    assert_eq!(
        commits.first(),
        Some(&RangeCommit {
            commit: merge.clone(),
            parents: vec![first.clone(), tip.clone()],
        })
    );
    assert_eq!(merged, MergeTree::Clean(objects.tree(&merge)?));
    assert!(
        objects.is_ancestor(&tip, &tip)?,
        "a commit is its own ancestor"
    );
    assert!(
        !objects.is_ancestor(&first, &tip)?,
        "the PR commit is not on the base"
    );
    assert_eq!(objects.range(&base, &merge, 2)?, RangeListing::TooMany);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_previous_head_reads_as_an_ancestor_a_rewrite_or_unreachable() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    fs::create_dir(&work)?;
    plain_git(&work, &["init", "--quiet", "--initial-branch=main"])?;
    fs::write(work.join("lib.rs"), "fn one() {}\n")?;
    let base = commit(&work, "base")?;
    plain_git(&work, &["checkout", "--quiet", "-b", "pr"])?;
    fs::write(work.join("pr.rs"), "fn pr() {}\n")?;
    let previous = commit(&work, "first")?;
    fs::write(work.join("pr.rs"), "fn pr() { two() }\n")?;
    let head = commit(&work, "second")?;
    plain_git(&work, &["checkout", "--quiet", "-b", "rewritten", "main"])?;
    fs::write(work.join("pr.rs"), "fn pr() { other() }\n")?;
    let rewritten = commit(&work, "rewritten")?;
    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git(&work, &["push", "--quiet", destination, "pr", "rewritten"])?;
    let objects = ForgePullRequest::new(&repo, TreeLimits::default(), Duration::from_secs(60));
    let absent: CommitId = "e".repeat(40).parse()?;
    let commits = RangeDiffCommits {
        base: &base,
        previous: &previous,
        head: &head,
    };

    let ancestries = [
        objects.ancestry(&previous, &head)?,
        objects.ancestry(&rewritten, &head)?,
        objects.ancestry(&absent, &head)?,
    ];
    let listing = objects.range_diff(commits, 1 << 20)?;
    let capped = objects.range_diff(commits, 1)?;

    assert_eq!(
        ancestries,
        [
            Ancestry::Ancestor,
            Ancestry::Rewritten,
            Ancestry::Unreachable
        ]
    );
    let RangeDiffListing::Listed(text) = listing else {
        return Err(format!("range-diff did not list: {listing:?}").into());
    };
    assert!(!text.is_empty(), "the second commit is new since the first");
    assert_eq!(capped, RangeDiffListing::TooLarge);
    Ok(())
}
