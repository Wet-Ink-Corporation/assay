//! A developer's working tree as data for local mode (02 §6): `WorkingTree` reads the
//! repository, `ObjectRepo::snapshot` records the dirty tree in a scratch repository that only
//! borrows the developer's objects, and `ObjectRepo::checkout` writes it out for the model.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

mod refusals;

use assay_forge::git::{
    CommitSha, EntryKind, GitEnvironment, GitError, GitLimits, ObjectRepo, Snapshot, TreeLimits,
    UserDirs, WorkingTree,
};
use tempfile::TempDir;

use crate::support::{commit, plain_git, write};

const GENEROUS: TreeLimits = TreeLimits {
    max_entries: 1000,
    max_total_bytes: 1 << 20,
};

/// A developer's clone: `work/` with one commit, plus a scratch directory for the snapshot and
/// an empty `home/` as the developer's `HOME`.
struct Clone {
    scratch: TempDir,
    root: PathBuf,
    head: CommitSha,
}

impl Clone {
    fn new() -> Result<Self, Box<dyn Error>> {
        let scratch = tempfile::tempdir()?;
        let root = scratch.path().join("work");
        plain_git(scratch.path(), &["init", "--quiet", "work"])?;
        fs::create_dir(scratch.path().join("home"))?;
        write(&root, ".gitignore", b"*.secret\n")?;
        write(&root, "src/lib.rs", b"pub fn one() {}\n")?;
        write(&root, "src/gone.rs", b"pub fn gone() {}\n")?;
        let head = commit(&root, "base")?;
        Ok(Self {
            scratch,
            root,
            head,
        })
    }

    /// The developer's `HOME`.
    fn home(&self) -> PathBuf {
        self.scratch.path().join("home")
    }

    /// The developer's user dirs: [`Clone::home`] and no `XDG_CONFIG_HOME`.
    fn user(&self) -> UserDirs {
        UserDirs::new(Some(self.home().into_os_string()), None)
    }

    fn open(&self) -> Result<WorkingTree, Box<dyn Error>> {
        Ok(WorkingTree::open(
            &self.root,
            GitEnvironment::from_process_env()?,
            &self.user(),
            GitLimits::default(),
        )?)
    }

    /// A fresh scratch repository under `name`.
    fn scratch_repo(&self, name: &str) -> Result<ObjectRepo, Box<dyn Error>> {
        Ok(ObjectRepo::init(
            self.scratch.path().join(name),
            GitEnvironment::from_process_env()?,
            GitLimits::default(),
        )?)
    }

    /// Opens the working tree and snapshots it into a fresh scratch repository `name`.
    fn snapshot(&self, name: &str) -> Result<(ObjectRepo, Snapshot), Box<dyn Error>> {
        let tree = self.open()?;
        let repo = self.scratch_repo(name)?;
        let snapshot = repo.snapshot(&tree, &self.head)?;
        Ok((repo, snapshot))
    }

    /// Every loose and packed object of the developer's repository, as git counts them.
    fn object_count(&self) -> Result<String, Box<dyn Error>> {
        plain_git(&self.root, &["count-objects", "-v"])
    }
}

/// The snapshot's files as path and content.
fn files(repo: &ObjectRepo, snapshot: &Snapshot) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let entries = repo.list_tree(snapshot.commit(), None, GENEROUS)?;
    let blobs = repo.read_blobs(
        &entries.iter().map(Into::into).collect::<Vec<_>>(),
        GENEROUS,
    )?;
    entries
        .iter()
        .zip(blobs)
        .map(|(entry, blob)| Ok((entry.path.clone(), String::from_utf8(blob)?)))
        .collect()
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_snapshot_holds_every_uncommitted_change_and_no_ignored_file() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    write(&clone.root, "src/lib.rs", b"pub fn two() {}\n")?;
    write(&clone.root, "src/new.rs", b"pub fn new() {}\n")?;
    fs::remove_file(clone.root.join("src/gone.rs"))?;
    write(&clone.root, "api.secret", b"token\n")?;

    let (repo, snapshot) = clone.snapshot("snap.git")?;

    assert_eq!(
        files(&repo, &snapshot)?,
        [
            (".gitignore".to_owned(), "*.secret\n".to_owned()),
            ("src/lib.rs".to_owned(), "pub fn two() {}\n".to_owned()),
            ("src/new.rs".to_owned(), "pub fn new() {}\n".to_owned()),
        ],
        "the snapshot is the tree `git add --all` would stage: edits, new files and deletions, \
         without ignored files"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_tracked_file_stays_in_the_snapshot_even_when_ignored() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    write(&clone.root, "kept.secret", b"tracked\n")?;
    plain_git(&clone.root, &["add", "--force", "kept.secret"])?;
    let clone = Clone {
        head: commit(&clone.root, "track an ignored name")?,
        ..clone
    };

    let (repo, snapshot) = clone.snapshot("snap.git")?;

    let paths: Vec<String> = files(&repo, &snapshot)?
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        paths,
        [".gitignore", "kept.secret", "src/gone.rs", "src/lib.rs"],
        "a tracked file is part of the push whatever .gitignore says"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_repository_exclude_file_is_honoured() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    fs::write(clone.root.join(".git/info/exclude"), "scratch.txt\n")?;
    write(&clone.root, "scratch.txt", b"notes\n")?;

    let (repo, snapshot) = clone.snapshot("snap.git")?;

    assert!(
        files(&repo, &snapshot)?
            .iter()
            .all(|(path, _)| path != "scratch.txt"),
        "a file excluded by .git/info/exclude is not part of the push"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_uncommitted_edit_changes_the_tree_fingerprint() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let (_, clean) = clone.snapshot("clean.git")?;
    let (_, again) = clone.snapshot("again.git")?;
    write(&clone.root, "src/lib.rs", b"pub fn edited() {}\n")?;

    let (_, dirty) = clone.snapshot("dirty.git")?;

    assert_eq!(
        (clean.tree(), clean.commit()),
        (again.tree(), again.commit()),
        "the same tree and HEAD give the same fingerprint and snapshot commit"
    );
    assert_ne!(
        clean.tree(),
        dirty.tree(),
        "an uncommitted edit changes the fingerprint"
    );
    let committed = plain_git(&clone.root, &["rev-parse", "HEAD^{tree}"])?;
    assert_eq!(
        clean.tree(),
        committed.trim(),
        "a clean tree's fingerprint is HEAD's tree id"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_snapshot_commit_sits_on_head() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    write(&clone.root, "src/lib.rs", b"pub fn two() {}\n")?;

    let (repo, snapshot) = clone.snapshot("snap.git")?;

    let parent = plain_git(
        repo.git_dir(),
        &["rev-parse", &format!("{}^", snapshot.commit())],
    )?;
    assert_eq!(
        parent.trim(),
        clone.head.as_str(),
        "the snapshot's parent is HEAD"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn snapshotting_writes_nothing_into_the_developer_repository() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    write(&clone.root, "src/lib.rs", b"pub fn two() {}\n")?;
    write(&clone.root, "src/new.rs", b"pub fn new() {}\n")?;
    let objects = clone.object_count()?;
    let status = plain_git(&clone.root, &["status", "--porcelain"])?;

    let (_, snapshot) = clone.snapshot("snap.git")?;

    assert_eq!(clone.object_count()?, objects, "no object was written");
    assert_eq!(
        plain_git(&clone.root, &["status", "--porcelain"])?,
        status,
        "the index and working tree are untouched"
    );
    let known = plain_git(&clone.root, &["cat-file", "-t", snapshot.tree()]);
    assert!(
        known.is_err(),
        "the snapshot's tree lives only in the scratch repository, got {known:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_linked_worktree_is_read_through_its_git_file() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let linked = clone.scratch.path().join("linked");
    let linked_text = linked.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git(
        &clone.root,
        &["worktree", "add", "--quiet", "--detach", linked_text],
    )?;
    write(&linked, "src/lib.rs", b"pub fn linked() {}\n")?;

    let tree = WorkingTree::open(
        &linked,
        GitEnvironment::from_process_env()?,
        &clone.user(),
        GitLimits::default(),
    )?;
    let head = tree.resolve("HEAD")?;
    let repo = clone.scratch_repo("snap.git")?;
    let snapshot = repo.snapshot(&tree, &head)?;

    assert_eq!(head, clone.head, "the linked worktree's HEAD is resolved");
    assert!(
        files(&repo, &snapshot)?
            .contains(&("src/lib.rs".to_owned(), "pub fn linked() {}\n".to_owned())),
        "the linked worktree's own edit is in the snapshot"
    );
    assert_eq!(
        tree.git_dir(),
        clone.root.join(".git/worktrees/linked").canonicalize()?,
        "a linked worktree's own git directory holds its local state"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn resolves_a_branch_and_refuses_an_unknown_revision() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(&clone.root, &["branch", "topic"])?;
    let tree = clone.open()?;

    let topic = tree.resolve("refs/heads/topic")?;
    let unknown = tree.resolve("refs/remotes/origin/main");

    assert_eq!(topic, clone.head, "a branch resolves to its commit");
    assert!(
        matches!(
            &unknown,
            Err(GitError::UnknownRevision { revision }) if revision == "refs/remotes/origin/main"
        ),
        "an unknown revision is named, got {unknown:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_unborn_head_is_an_unknown_revision() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    plain_git(scratch.path(), &["init", "--quiet", "fresh"])?;
    let tree = WorkingTree::open(
        scratch.path().join("fresh"),
        GitEnvironment::from_process_env()?,
        &UserDirs::default(),
        GitLimits::default(),
    )?;

    let head = tree.resolve("HEAD");

    assert!(
        matches!(&head, Err(GitError::UnknownRevision { revision }) if revision == "HEAD"),
        "a repository without a commit has no HEAD to snapshot on, got {head:?}"
    );
    Ok(())
}

/// What `WorkingTree::open` says about `root`.
fn open_error(root: &Path) -> Result<String, Box<dyn Error>> {
    match WorkingTree::open(
        root,
        GitEnvironment::from_process_env()?,
        &UserDirs::default(),
        GitLimits::default(),
    ) {
        Err(GitError::NotAWorkingTree { reason, .. }) => Ok(reason.to_owned()),
        other => Err(format!("expected NotAWorkingTree, got {other:?}").into()),
    }
}

#[test]
#[cfg_attr(miri, ignore = "touches the file system")]
fn a_directory_without_git_is_refused() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;

    assert_eq!(open_error(scratch.path())?, "it has no .git");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "touches the file system")]
fn a_relative_root_is_refused() -> Result<(), Box<dyn Error>> {
    assert_eq!(open_error(Path::new("work"))?, "the path is not absolute");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "touches the file system")]
fn a_git_file_that_is_not_a_gitdir_line_is_refused() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    fs::write(scratch.path().join(".git"), "worktree: elsewhere\n")?;

    assert_eq!(
        open_error(scratch.path())?,
        "its .git file is not one `gitdir: <path>` line"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "touches the file system")]
fn a_git_file_at_the_size_cap_is_read_and_one_byte_over_is_refused() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    // `gitdir: ` plus a path padded to exactly 4 KiB with a trailing newline.
    let line = |len: usize| {
        let mut text = "gitdir: ".to_owned();
        text.push_str(&"/".repeat(len.saturating_sub(text.len() + 1)));
        text.push('\n');
        text
    };
    fs::write(scratch.path().join(".git"), line(4096))?;
    let at_cap = open_error(scratch.path())?;
    fs::write(scratch.path().join(".git"), line(4097))?;
    let over = open_error(scratch.path())?;

    // At the cap the file is read, and `/` (the root) is not a repository git accepts.
    assert_eq!(at_cap, "git does not read it as a repository");
    assert_eq!(over, "its .git file is over 4096 bytes");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn checkout_writes_exactly_the_snapshot_with_symlinks_as_files() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    write(&clone.root, "src/new.rs", b"pub fn new() {}\n")?;
    write(&clone.root, "api.secret", b"token\n")?;
    std::os::unix::fs::symlink("/etc/hostname", clone.root.join("host"))?;
    let (repo, snapshot) = clone.snapshot("snap.git")?;
    let dest = clone.scratch.path().join("tree");

    repo.checkout(&snapshot, &dest)?;

    let mut written = Vec::new();
    for entry in walk(&dest)? {
        let kind = fs::symlink_metadata(&entry)?.file_type();
        let relative = entry.strip_prefix(&dest)?.to_string_lossy().into_owned();
        written.push((relative, kind.is_file()));
    }
    written.sort();
    assert_eq!(
        written,
        [
            (".gitignore".to_owned(), true),
            ("host".to_owned(), true),
            ("src/gone.rs".to_owned(), true),
            ("src/lib.rs".to_owned(), true),
            ("src/new.rs".to_owned(), true),
        ],
        "every snapshot file and nothing else, a symlink written as a plain file"
    );
    assert_eq!(fs::read_to_string(dest.join("host"))?, "/etc/hostname");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn checkout_refuses_a_destination_with_contents() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let (repo, snapshot) = clone.snapshot("snap.git")?;
    let dest = clone.scratch.path().join("tree");
    write(&dest, "stale.txt", b"left over\n")?;

    let result = repo.checkout(&snapshot, &dest);

    assert!(
        matches!(&result, Err(GitError::DirectoryNotEmpty { git_dir }) if *git_dir == dest),
        "a stale file must not mix into the model's tree, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_scratch_repository_takes_one_snapshot() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let tree = clone.open()?;
    let repo = clone.scratch_repo("snap.git")?;
    repo.snapshot(&tree, &clone.head)?;

    let again = repo.snapshot(&tree, &clone.head);

    assert!(
        matches!(&again, Err(GitError::Write { .. })),
        "the borrowed object store is set once, got {again:?}"
    );
    Ok(())
}

/// Every non-directory under `dir`.
fn walk(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            found.extend(walk(&path)?);
        } else {
            found.push(path);
        }
    }
    Ok(found)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_snapshot_lists_its_files_as_regular_entries() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let (repo, snapshot) = clone.snapshot("snap.git")?;

    let entries = repo.list_tree(snapshot.commit(), Some("src"), GENEROUS)?;

    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.kind))
            .collect::<Vec<_>>(),
        [
            ("src/gone.rs", EntryKind::File),
            ("src/lib.rs", EntryKind::File)
        ],
        "the snapshot is read like any fetched commit"
    );
    Ok(())
}
