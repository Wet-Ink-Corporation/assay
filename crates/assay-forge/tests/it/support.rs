//! Fixture repositories: a base and a head commit built with plain git in a scratch worktree,
//! then pushed into an object-only [`ObjectRepo`], the way Z4 receives PR objects.

use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assay_forge::git::{CommitSha, FileChange, GitEnvironment, GitLimits, ObjectRepo};
use tempfile::TempDir;

/// A repository holding a base commit and a head commit on top of it.
pub(crate) struct Fixture {
    /// Keeps the scratch directory alive for the test, and holds the bare repository.
    scratch: TempDir,
    pub(crate) repo: ObjectRepo,
    pub(crate) base: CommitSha,
    pub(crate) head: CommitSha,
}

impl Fixture {
    /// The bare repository's directory.
    pub(crate) fn git_dir(&self) -> PathBuf {
        self.scratch.path().join("objects.git")
    }

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
    fixture_with_head(|work| {
        for (path, contents) in head_files {
            write(work, path, contents)?;
        }
        for path in removed {
            fs::remove_file(work.join(path))?;
        }
        Ok(())
    })
}

/// Like [`fixture`], with the head commit made by `edit` in the scratch worktree.
pub(crate) fn fixture_with_head(
    edit: impl FnOnce(&Path) -> Result<(), Box<dyn Error>>,
) -> Result<Fixture, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    write(&work, "README.md", b"# fixture\n")?;
    write(&work, "Cargo.toml", b"[package]\nname = \"fixture\"\n")?;
    write(&work, "src/lib.rs", b"pub fn one() -> u8 {\n    1\n}\n")?;
    write(&work, "legacy/kept.rs", b"pub fn kept() {}\n")?;
    let base = commit(&work, "base")?;
    edit(&work)?;
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
        scratch,
        repo,
        base,
        head,
    })
}

/// A base branch that moved on after the PR forked from it: the fork point holds the files of
/// [`fixture`], `base_files` are committed on top of it as the base commit, and `head_files`
/// on top of the fork point as the head commit, so the base is not an ancestor of the head.
pub(crate) fn forked_fixture(
    base_files: &[(&str, &[u8])],
    head_files: &[(&str, &[u8])],
) -> Result<Fixture, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    write(&work, "README.md", b"# fixture\n")?;
    write(&work, "Cargo.toml", b"[package]\nname = \"fixture\"\n")?;
    write(&work, "src/lib.rs", b"pub fn one() -> u8 {\n    1\n}\n")?;
    let fork = commit(&work, "fork")?;
    for (path, contents) in base_files {
        write(&work, path, contents)?;
    }
    let base = commit(&work, "base")?;
    plain_git(&work, &["checkout", "--quiet", "-b", "pr", fork.as_str()])?;
    for (path, contents) in head_files {
        write(&work, path, contents)?;
    }
    let head = commit(&work, "head")?;

    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    let base_ref = format!("{base}:refs/heads/base");
    plain_git(
        &work,
        &[
            "push",
            "--quiet",
            destination,
            "pr:refs/heads/pr",
            &base_ref,
        ],
    )?;
    Ok(Fixture {
        scratch,
        repo,
        base,
        head,
    })
}

/// A force-pushed PR: the base holds the files of [`fixture`]; the earlier head (`previous`,
/// committed as "previous") and the head that replaced it ("head") both sit on the base, so the
/// earlier head is reachable but not an ancestor of the head.
pub(crate) struct Forced {
    pub(crate) fixture: Fixture,
    pub(crate) previous: CommitSha,
}

pub(crate) fn forced_fixture() -> Result<Forced, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    write(&work, "README.md", b"# fixture\n")?;
    write(&work, "src/lib.rs", b"pub fn one() -> u8 {\n    1\n}\n")?;
    let base = commit(&work, "base")?;
    write(&work, "src/lib.rs", b"pub fn one() -> u8 {\n    2\n}\n")?;
    let previous = commit(&work, "previous")?;
    plain_git(&work, &["branch", "earlier"])?;
    plain_git(&work, &["reset", "--quiet", "--hard", base.as_str()])?;
    write(&work, "src/lib.rs", b"pub fn one() -> u8 {\n    3\n}\n")?;
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
        &[
            "push",
            "--quiet",
            destination,
            "HEAD:refs/heads/pr",
            "earlier:refs/heads/earlier",
        ],
    )?;
    Ok(Forced {
        fixture: Fixture {
            scratch,
            repo,
            base,
            head,
        },
        previous,
    })
}

/// A PR of `commits` empty commits on top of a base holding a README, written in one
/// `git fast-import` so a long history is cheap to build.
pub(crate) fn long_fixture(commits: u64) -> Result<Fixture, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    write(&work, "README.md", b"# fixture\n")?;
    let base = commit(&work, "base")?;
    let mut stream = String::new();
    for n in 1..=commits {
        let from = if n == 1 {
            base.to_string()
        } else {
            format!(":{}", n - 1)
        };
        writeln!(
            stream,
            "commit refs/heads/long\nmark :{n}\ncommitter fixture <fixture@example.invalid> \
             {n} +0000\ndata 6\nstep {}\nfrom {from}\n",
            n % 10
        )?;
    }
    let mut import = Command::new("git");
    import.env_clear();
    if let Some(path) = env::var_os("PATH") {
        import.env("PATH", path);
    }
    let mut child = import
        .current_dir(&work)
        .args(["fast-import", "--quiet"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    std::io::Write::write_all(
        &mut child.stdin.take().ok_or("fast-import has no stdin")?,
        stream.as_bytes(),
    )?;
    if !child.wait()?.success() {
        return Err("git fast-import failed".into());
    }
    let head: CommitSha = plain_git(&work, &["rev-parse", "long"])?.trim().parse()?;

    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git(
        &work,
        &["push", "--quiet", destination, "long:refs/heads/pr"],
    )?;
    Ok(Fixture {
        scratch,
        repo,
        base,
        head,
    })
}

pub(crate) fn write(work: &Path, path: &str, contents: &[u8]) -> Result<(), Box<dyn Error>> {
    let path = work.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

pub(crate) fn commit(work: &Path, message: &str) -> Result<CommitSha, Box<dyn Error>> {
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

/// `read`'s value, or `None` when it has not returned within ten seconds, so a test of a
/// FIFO that must not block fails instead of hanging. A reader stuck opening `fifo` is then
/// freed by opening it for writing, so the scoped thread is joined either way (H-13).
pub(crate) fn within_bound<T: Send>(
    fifo: &Path,
    read: impl FnOnce() -> T + Send,
) -> Result<Option<T>, Box<dyn Error>> {
    std::thread::scope(|scope| {
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("fifo-read".to_owned())
            .spawn_scoped(scope, move || sent.send(read()))?;
        match received.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(value) => Ok(Some(value)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                drop(fs::OpenOptions::new().write(true).open(fifo)?);
                Ok(None)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(None),
        }
    })
}

/// Writes `text` as an executable (mode 0755) at `path`, which must not exist.
///
/// A child `install` writes the executable, so no thread of this test binary ever holds it
/// open for writing: a sibling test's fork would inherit that descriptor until its exec, and
/// executing the file meanwhile fails with ETXTBSY (#176). The text goes to a `.source` file
/// beside `path` and the copy to a `.staged` one, renamed into place, so `path` is complete
/// whenever it exists.
pub(crate) fn write_executable(path: &Path, text: &str) -> Result<(), Box<dyn Error>> {
    let source = path.with_extension("source");
    let staged = path.with_extension("staged");
    fs::write(&source, text)?;
    let installed = Command::new("install")
        .args(["-m", "0755"])
        .arg(&source)
        .arg(&staged)
        .status()?;
    if !installed.success() {
        return Err(format!("install {} failed: {installed}", staged.display()).into());
    }
    fs::rename(&staged, path)?;
    fs::remove_file(&source)?;
    Ok(())
}
