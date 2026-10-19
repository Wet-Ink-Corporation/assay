//! A developer's working tree as data, for local mode (`docs/architecture/02-architecture.md`
//! §6): [`WorkingTree`] reads the repository, [`ObjectRepo::snapshot`] records the dirty tree as
//! a commit in a scratch repository, and [`ObjectRepo::checkout`] writes that tree out.
//!
//! The developer's repository is only read: the scratch repository borrows its object store
//! through `objects/info/alternates`, and every object the snapshot writes, and its index, live
//! in the scratch repository. Every git process is the hardened constructor's, so no hook,
//! fsmonitor, attribute or filter applies: files are recorded byte for byte.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

mod developer;
mod location;

use self::developer::Sparse;
use super::GitError;
use super::command::{GitEnvironment, GitOperation, Scope, UserDirs};
use super::repository::{Call, GitLimits, ObjectRepo, call, run_in};
use super::sha::CommitSha;
use super::tree::is_object_id;
use crate::process::Captured;

/// Largest `.git` file read: a linked worktree's `gitdir: <path>` line.
const MAX_GIT_FILE_BYTES: u64 = 4096;

/// Most bytes of exclude rules copied into the scratch repository: the developer's global
/// excludes file and `info/exclude` together.
const MAX_EXCLUDE_BYTES: u64 = 1 << 20;

/// The scratch index the snapshot is staged in, inside the scratch repository.
const SNAPSHOT_INDEX: &str = "assay-snapshot-index";

/// The scratch index a checkout is written from, inside the scratch repository.
const CHECKOUT_INDEX: &str = "assay-checkout-index";

/// The message of every snapshot commit.
const SNAPSHOT_MESSAGE: &str = "assay local snapshot";

/// A developer's repository and working tree, read for local mode. Nothing here writes to it.
#[derive(Debug)]
pub struct WorkingTree {
    root: PathBuf,
    git_dir: PathBuf,
    objects: PathBuf,
    exclude: PathBuf,
    /// The developer's global excludes file, when their git has one.
    excludes_file: Option<PathBuf>,
    environment: GitEnvironment,
    limits: GitLimits,
}

/// A working tree recorded in a scratch repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    tree: String,
    commit: CommitSha,
}

impl Snapshot {
    /// The tree id: the fingerprint of the working tree as `git add --all` would stage it on top
    /// of `HEAD`, so every uncommitted change to a file the push could carry changes it.
    #[must_use]
    pub fn tree(&self) -> &str {
        &self.tree
    }

    /// A commit of the tree on top of `HEAD`, with a fixed identity and date, so its id is a
    /// function of the tree and `HEAD` alone. It exists only in the scratch repository.
    #[must_use]
    pub const fn commit(&self) -> &CommitSha {
        &self.commit
    }
}

impl WorkingTree {
    /// The repository whose working tree is `root`: `root/.git` is its git directory, or a
    /// `gitdir: <path>` file naming one (a linked worktree). Its object store and
    /// `info/exclude` are asked of git, so a linked worktree finds the shared ones.
    /// The developer's global excludes file is looked up with `user`'s `HOME` and
    /// `XDG_CONFIG_HOME`, as their own git looks it up.
    ///
    /// # Errors
    /// [`GitError::NotAWorkingTree`] for a relative `root`, a `root` without `.git`, a `.git`
    /// that is a symlink or a file other than one `gitdir:` line of at most 4096 bytes, a git
    /// directory git does not accept, a sparse working tree (`core.sparseCheckout` set, or
    /// skip-worktree entries), or a global excludes file that is not
    /// named by an absolute path; [`GitError`] when git cannot run, fails, or prints something
    /// this layer cannot read.
    pub fn open(
        root: impl Into<PathBuf>,
        environment: GitEnvironment,
        user: &UserDirs,
        limits: GitLimits,
    ) -> Result<Self, GitError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(not_a_tree(root, "the path is not absolute"));
        }
        let named = match dot_git(&root) {
            Ok(named) => named,
            Err(reason) => return Err(not_a_tree(root, reason)),
        };
        let args = [
            "--absolute-git-dir",
            "--git-path",
            "objects",
            "--git-path",
            "info/exclude",
        ];
        let captured = match run_in(
            &named,
            &environment,
            limits,
            call(GitOperation::RevParse, &args, limits),
        ) {
            Err(GitError::Failed { .. }) => {
                return Err(not_a_tree(root, "git does not read it as a repository"));
            }
            other => other?,
        };
        let [git_dir, objects, exclude] = absolute_lines(&captured.stdout)?;
        let mut tree = Self {
            root,
            git_dir,
            objects,
            exclude,
            excludes_file: None,
            environment,
            limits,
        };
        if let Sparse::Yes(reason) = tree.sparse(user)? {
            return Err(not_a_tree(tree.root, reason));
        }
        tree.excludes_file = tree.excludes_file(user)?;
        Ok(tree)
    }

    /// The working tree's root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The working tree's own git directory (`root/.git`, or a linked worktree's directory
    /// under the shared one), where local state belongs.
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The commit `revision` names (`HEAD`, `refs/remotes/origin/main`), read as a revision
    /// only, never as an option.
    ///
    /// # Errors
    /// [`GitError::UnknownRevision`] when it names no commit (an unborn `HEAD` included), and
    /// [`GitError`] when git cannot run or prints something other than one commit id.
    pub fn resolve(&self, revision: &str) -> Result<CommitSha, GitError> {
        let peeled = format!("{revision}^{{commit}}");
        let args = ["--verify", "--quiet", "--end-of-options", peeled.as_str()];
        let captured = match self.run(call(GitOperation::RevParse, &args, self.limits)) {
            Err(GitError::Failed { .. }) => {
                return Err(GitError::UnknownRevision {
                    // The error owns the revision it names.
                    revision: revision.to_owned(),
                });
            }
            other => other?,
        };
        one_id(&captured.stdout, GitOperation::RevParse)?
            .parse()
            .map_err(|_sha| GitError::Malformed {
                operation: GitOperation::RevParse,
                detail: "rev-parse did not print one commit id",
            })
    }

    fn run(&self, call: Call<'_>) -> Result<Captured, GitError> {
        run_in(&self.git_dir, &self.environment, self.limits, call)
    }
}

impl ObjectRepo {
    /// Records `worktree` on top of `head` in this repository: it borrows the developer's
    /// object store, global excludes file and `info/exclude`, loads `head`'s tree into a
    /// scratch index, stages the working tree with `git add --all` (edits, deletions, and
    /// untracked files `.gitignore` and the exclude files do not ignore; a tracked file stays
    /// whatever they say), and writes the tree and a commit of it. Only this repository is
    /// written. Call it once per repository.
    ///
    /// # Errors
    /// [`GitError::Write`] when the borrowed store or exclude file cannot be set up (including
    /// on a second call), [`GitError::Inspect`] when an exclude file cannot be read or is not
    /// a regular file (a FIFO or device is never opened), [`GitError::NotAWorkingTree`] when
    /// the exclude files hold over 1 MiB together or the object store's path holds a newline,
    /// and [`GitError`] when a git step cannot run, fails or prints something other than one
    /// object id.
    pub fn snapshot(&self, worktree: &WorkingTree, head: &CommitSha) -> Result<Snapshot, GitError> {
        self.borrow_objects(worktree)?;
        self.copy_exclude(worktree)?;
        let index = self.git_dir().join(SNAPSHOT_INDEX);
        let staging = Scope {
            work_tree: Some(worktree.root()),
            index: Some(&index),
            ..Scope::default()
        };
        self.run(Call {
            scope: staging,
            ..self.call(GitOperation::ReadTree, &[head.as_str()])
        })?;
        self.run(Call {
            scope: staging,
            ..self.call(GitOperation::AddAll, &[])
        })?;
        let written = self.run(Call {
            scope: staging,
            ..self.call(GitOperation::WriteTree, &[])
        })?;
        let tree = one_id(&written.stdout, GitOperation::WriteTree)?.to_owned();
        let args = [tree.as_str(), "-p", head.as_str(), "-m", SNAPSHOT_MESSAGE];
        let committed = self.run(self.call(GitOperation::CommitTree, &args))?;
        let commit = one_id(&committed.stdout, GitOperation::CommitTree)?
            .parse()
            .map_err(|_sha| GitError::Malformed {
                operation: GitOperation::CommitTree,
                detail: "commit-tree did not print one commit id",
            })?;
        Ok(Snapshot { tree, commit })
    }

    /// Writes `snapshot`'s files into `dest`, which must be absent or empty, every symlink as a
    /// plain file holding its target, so nothing read from `dest` follows a link out of it.
    ///
    /// # Errors
    /// [`GitError::DirectoryNotEmpty`] when `dest` has contents, [`GitError::Inspect`] when it
    /// cannot be read or created, and [`GitError`] when a git step cannot run or fails.
    pub fn checkout(&self, snapshot: &Snapshot, dest: &Path) -> Result<(), GitError> {
        match fs::read_dir(dest) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(GitError::DirectoryNotEmpty {
                        git_dir: dest.to_path_buf(),
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(dest).map_err(|source| GitError::Inspect {
                    git_dir: dest.to_path_buf(),
                    source,
                })?;
            }
            Err(source) => {
                return Err(GitError::Inspect {
                    git_dir: dest.to_path_buf(),
                    source,
                });
            }
        }
        let index = self.git_dir().join(CHECKOUT_INDEX);
        let scope = Scope {
            work_tree: Some(dest),
            index: Some(&index),
            ..Scope::default()
        };
        self.run(Call {
            scope,
            ..self.call(GitOperation::ReadTree, &[snapshot.tree()])
        })?;
        self.run(Call {
            scope,
            ..self.call(GitOperation::CheckoutIndex, &[])
        })?;
        Ok(())
    }

    /// Points this repository's `objects/info/alternates` at the developer's object store.
    fn borrow_objects(&self, worktree: &WorkingTree) -> Result<(), GitError> {
        let Some(line) = alternates_line(&worktree.objects) else {
            return Err(not_a_tree(
                // The error owns the root it names; the working tree keeps its own.
                worktree.root.clone(),
                "its object store's path holds a newline",
            ));
        };
        let path = self.git_dir().join("objects/info/alternates");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(&line))
            .map_err(|source| GitError::Write { path, source })
    }

    /// Writes the developer's global excludes file, then their `info/exclude` (later rules
    /// win, as in git), over this repository's `info/exclude`, which `git add` reads. Together
    /// they may hold at most 1 MiB; an absent file adds nothing, as it does for git.
    fn copy_exclude(&self, worktree: &WorkingTree) -> Result<(), GitError> {
        let global = match &worktree.excludes_file {
            Some(path) => read_exclude(path, MAX_EXCLUDE_BYTES)?,
            None => Some(Vec::new()),
        };
        let local = global.as_ref().map_or(Ok(None), |global| {
            let left = u64::try_from(global.len())
                .ok()
                .and_then(|used| MAX_EXCLUDE_BYTES.checked_sub(used))
                .unwrap_or(0);
            read_exclude(&worktree.exclude, left)
        })?;
        let (Some(mut text), Some(local)) = (global, local) else {
            return Err(not_a_tree(
                // The error owns the root it names.
                worktree.root.clone(),
                "its exclude files are over 1 MiB together",
            ));
        };
        if !text.is_empty() && !local.is_empty() {
            text.push(b'\n');
        }
        text.extend(local);
        let path = self.git_dir().join("info/exclude");
        fs::create_dir_all(self.git_dir().join("info"))
            .and_then(|()| fs::write(&path, text))
            .map_err(|source| GitError::Write { path, source })
    }
}

/// The exclude rules at `path`: empty when it does not exist or is `/dev/null`, `None` when
/// over `cap` bytes.
///
/// `core.excludesFile = /dev/null` is the usual way to turn global excludes off and git reads
/// it as no rules, so that exact path is no rules and is not opened. Otherwise only a regular
/// file (or a symlink to one, which git follows too) is read. Anything else, another device
/// included, is refused before it is opened, since `open()` itself blocks on a FIFO with no
/// writer and a read of `/dev/tty` waits on the terminal (#219 R1); the handle is checked
/// again after the open. A path swapped for a FIFO between the check and the open is a
/// residual: closing it needs `O_NONBLOCK`, that is `libc` (H-16), as for local mode's report
/// store.
fn read_exclude(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, GitError> {
    let inspect = |source| GitError::Inspect {
        git_dir: path.to_path_buf(),
        source,
    };
    if path == Path::new("/dev/null") {
        return Ok(Some(Vec::new()));
    }
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(inspect(not_a_regular_file())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
        Err(source) => return Err(inspect(source)),
    }
    let file = File::open(path).map_err(inspect)?;
    if !file.metadata().map_err(inspect)?.is_file() {
        return Err(inspect(not_a_regular_file()));
    }
    read_capped(file, cap).map_err(inspect)
}

/// Why an exclude path that is not a regular file is not read.
fn not_a_regular_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
}

/// The `objects/info/alternates` line naming `objects`; `None` when the path holds a newline,
/// which would end the line early and name another store.
fn alternates_line(objects: &Path) -> Option<Vec<u8>> {
    let bytes = objects.as_os_str().as_bytes();
    if bytes.contains(&b'\n') {
        return None;
    }
    let mut line = bytes.to_vec();
    line.push(b'\n');
    Some(line)
}

/// The git directory `root/.git` names, or why it names none.
fn dot_git(root: &Path) -> Result<PathBuf, &'static str> {
    let dot_git = root.join(".git");
    let kind = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err("it has no .git"),
        Err(_unreadable) => return Err("its .git cannot be inspected"),
    };
    if kind.is_dir() {
        return Ok(dot_git);
    }
    if !kind.is_file() {
        return Err("its .git is neither a directory nor a file");
    }
    let file = File::open(&dot_git).map_err(|_unreadable| "its .git file cannot be read")?;
    let bytes = read_capped(file, MAX_GIT_FILE_BYTES)
        .map_err(|_unreadable| "its .git file cannot be read")?
        .ok_or("its .git file is over 4096 bytes")?;
    let named = bytes
        .strip_prefix(b"gitdir: ")
        .map(|rest| rest.strip_suffix(b"\n").unwrap_or(rest))
        .filter(|path| !path.is_empty() && !path.contains(&b'\n') && !path.contains(&0))
        .ok_or("its .git file is not one `gitdir: <path>` line")?;
    // A relative path is relative to the directory holding the `.git` file.
    Ok(root.join(PathBuf::from(OsString::from_vec(named.to_vec()))))
}

/// `reader`'s bytes, or `None` when it holds more than `cap`.
fn read_capped(reader: impl Read, cap: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    Ok(u64::try_from(bytes.len())
        .is_ok_and(|len| len <= cap)
        .then_some(bytes))
}

/// The `N` absolute paths `rev-parse` printed, one per line.
fn absolute_lines<const N: usize>(stdout: &[u8]) -> Result<[PathBuf; N], GitError> {
    let malformed = GitError::Malformed {
        operation: GitOperation::RevParse,
        detail: "rev-parse did not print one absolute path per question",
    };
    let Some(body) = stdout.strip_suffix(b"\n") else {
        return Err(malformed);
    };
    let paths: Vec<PathBuf> = body
        .split(|&byte| byte == b'\n')
        .map(|line| PathBuf::from(OsString::from_vec(line.to_vec())))
        .collect();
    match <[PathBuf; N]>::try_from(paths) {
        Ok(paths) if paths.iter().all(|path| path.is_absolute()) => Ok(paths),
        _ => Err(malformed),
    }
}

/// The one object id `stdout` holds, newline-terminated.
fn one_id(stdout: &[u8], operation: GitOperation) -> Result<&str, GitError> {
    str::from_utf8(stdout)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .filter(|id| is_object_id(id))
        .ok_or(GitError::Malformed {
            operation,
            detail: "git did not print one object id",
        })
}

fn not_a_tree(root: PathBuf, reason: &'static str) -> GitError {
    GitError::NotAWorkingTree { root, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `read`'s value, or `None` when it has not returned within ten seconds, so a FIFO test
    /// fails instead of hanging. A reader stuck opening `fifo` is then freed by opening it for
    /// writing, so the scoped thread is joined either way (H-13).
    fn within_bound<T: Send>(
        fifo: &Path,
        read: impl FnOnce() -> T + Send,
    ) -> io::Result<Option<T>> {
        std::thread::scope(|scope| {
            let (sent, received) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("read-exclude".to_owned())
                .spawn_scoped(scope, move || sent.send(read()))?;
            match received.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(value) => Ok(Some(value)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    drop(OpenOptions::new().write(true).open(fifo)?);
                    Ok(None)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(None),
            }
        })
    }

    #[test]
    fn reads_at_the_cap_and_refuses_one_byte_more() -> io::Result<()> {
        let at_cap = read_capped(&b"abcd"[..], 4)?;
        let over = read_capped(&b"abcde"[..], 4)?;

        assert_eq!(at_cap, Some(b"abcd".to_vec()));
        assert_eq!(over, None);
        Ok(())
    }

    #[test]
    #[cfg_attr(miri, ignore = "runs mkfifo; Miri cannot spawn processes")]
    fn a_fifo_excludes_file_is_refused_without_blocking() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let fifo = dir.path().join("ignore");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
        assert!(made.success(), "mkfifo {}", fifo.display());

        let result = within_bound(&fifo, || read_exclude(&fifo, MAX_EXCLUDE_BYTES))?
            .ok_or("read_exclude blocked on the FIFO")?;

        assert!(
            matches!(&result, Err(GitError::Inspect { source, .. })
                if source.kind() == io::ErrorKind::InvalidInput),
            "a FIFO is refused before it is opened, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn dev_null_as_the_excludes_file_holds_no_rules() {
        let result = read_exclude(Path::new("/dev/null"), MAX_EXCLUDE_BYTES);

        assert!(
            matches!(&result, Ok(Some(rules)) if rules.is_empty()),
            "/dev/null turns global excludes off, as in git, got {result:?}"
        );
    }

    #[test]
    fn a_device_excludes_file_other_than_dev_null_is_refused() {
        let result = read_exclude(Path::new("/dev/zero"), MAX_EXCLUDE_BYTES);

        assert!(
            matches!(&result, Err(GitError::Inspect { source, .. })
                if source.kind() == io::ErrorKind::InvalidInput),
            "only a regular file or /dev/null holds exclude rules, got {result:?}"
        );
    }

    #[test]
    fn an_object_store_path_with_a_newline_has_no_alternates_line() {
        assert_eq!(
            alternates_line(Path::new("/repo/.git/objects")),
            Some(b"/repo/.git/objects\n".to_vec())
        );
        assert_eq!(alternates_line(Path::new("/repo\n/etc/objects")), None);
    }

    #[test]
    fn three_absolute_lines_are_paths_and_anything_else_is_malformed() {
        let read = absolute_lines::<3>(b"/a\n/b\n/c\n");
        let relative = absolute_lines::<3>(b"/a\nb\n/c\n");
        let short = absolute_lines::<3>(b"/a\n/b\n");

        assert!(
            matches!(&read, Ok([a, b, c]) if *a == Path::new("/a") && *b == Path::new("/b")
                && *c == Path::new("/c")),
            "{read:?}"
        );
        assert!(
            matches!(relative, Err(GitError::Malformed { .. })),
            "{relative:?}"
        );
        assert!(
            matches!(short, Err(GitError::Malformed { .. })),
            "{short:?}"
        );
    }
}
