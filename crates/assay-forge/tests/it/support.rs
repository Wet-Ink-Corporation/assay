//! Fixture repositories: a base and a head commit built with plain git in a scratch worktree,
//! then pushed into an object-only [`ObjectRepo`], the way Z4 receives PR objects.

use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use assay_forge::git::{CommitSha, FileChange, GitEnvironment, GitLimits, ObjectRepo};
use tempfile::TempDir;

/// A repository holding a base commit and a head commit on top of it.
pub(crate) struct Fixture {
    /// Keeps the scratch directory alive for the test.
    _scratch: TempDir,
    pub(crate) repo: ObjectRepo,
    pub(crate) base: CommitSha,
    pub(crate) head: CommitSha,
}

impl Fixture {
    /// The files the head commit changed, as numstat reports them.
    pub(crate) fn changes(&self) -> Result<Vec<FileChange>, Box<dyn Error>> {
        let outcome =
            self.repo
                .numstat(&self.base, &self.head, std::time::Duration::from_secs(60))?;
        match outcome {
            assay_forge::git::DiffOutcome::Measured(changes) => Ok(changes),
            assay_forge::git::DiffOutcome::NeedsSplit => {
                Err("the fixture diff did not finish".into())
            }
        }
    }
}

/// The base commit holds a README and a small crate; `head_files` are written on top of it.
pub(crate) fn fixture(head_files: &[(&str, &[u8])]) -> Result<Fixture, Box<dyn Error>> {
    fixture_with_removals(head_files, &[])
}

/// Like [`fixture`], and the head commit also deletes `removed` (paths of the base commit or
/// of `head_files`, which then become renames when the contents match).
pub(crate) fn fixture_with_removals(
    head_files: &[(&str, &[u8])],
    removed: &[&str],
) -> Result<Fixture, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    write(&work, "README.md", b"# fixture\n")?;
    write(&work, "Cargo.toml", b"[package]\nname = \"fixture\"\n")?;
    write(&work, "src/lib.rs", b"pub fn one() -> u8 {\n    1\n}\n")?;
    write(&work, "legacy/kept.rs", b"pub fn kept() {}\n")?;
    let base = commit(&work, "base")?;
    for (path, contents) in head_files {
        write(&work, path, contents)?;
    }
    for path in removed {
        fs::remove_file(work.join(path))?;
    }
    let head = commit(&work, "head")?;

    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git(
        &work,
        &["push", "--quiet", destination, "HEAD:refs/heads/pr"],
    )?;
    // HEAD names the PR, so a bare repository would read its `.gitattributes` by default.
    plain_git(&git_dir, &["symbolic-ref", "HEAD", "refs/heads/pr"])?;
    Ok(Fixture {
        _scratch: scratch,
        repo,
        base,
        head,
    })
}

fn write(work: &Path, path: &str, contents: &[u8]) -> Result<(), Box<dyn Error>> {
    let path = work.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

fn commit(work: &Path, message: &str) -> Result<CommitSha, Box<dyn Error>> {
    plain_git(work, &["add", "--all"])?;
    plain_git(work, &["commit", "--quiet", "--allow-empty", "-m", message])?;
    let sha = plain_git(work, &["rev-parse", "HEAD"])?;
    Ok(sha.trim().parse()?)
}

/// Runs git the way a developer would, isolated from the machine's own config and environment,
/// and returns its stdout. Test setup only: production code reaches git solely through
/// `ObjectRepo`.
pub(crate) fn plain_git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
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
