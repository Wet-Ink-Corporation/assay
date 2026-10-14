//! What local mode honours of the developer's own setup, and what it refuses rather than guess
//! (02 §6, wi-b5affa): the global excludes file, sparse working trees, odd `.git` entries and
//! exclude files over their cap.

use std::error::Error;
use std::fs;
use std::path::Path;

use assay_forge::git::{GitEnvironment, GitError, GitLimits, UserDirs, WorkingTree};

use super::{Clone, files, open_error};
use crate::support::{plain_git, write};

/// The developer's `~/.gitconfig` sets `core.excludesFile` to `value`.
fn set_excludes_file(clone: &Clone, value: &str) -> Result<(), Box<dyn Error>> {
    let config = format!("[core]\n\texcludesFile = {value}\n");
    fs::write(clone.home().join(".gitconfig"), config)?;
    Ok(())
}

/// The snapshot's paths.
fn snapshot_paths(clone: &Clone) -> Result<Vec<String>, Box<dyn Error>> {
    let (repo, snapshot) = clone.snapshot("snap.git")?;
    Ok(files(&repo, &snapshot)?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

/// What `WorkingTree::open` says about `clone` with its user dirs.
fn refusal(clone: &Clone) -> Result<String, Box<dyn Error>> {
    match clone.open() {
        Err(error) => match *error.downcast::<GitError>()? {
            error @ GitError::NotAWorkingTree { .. } => Ok(error.to_string()),
            other => Err(format!("expected NotAWorkingTree, got {other:?}").into()),
        },
        Ok(tree) => Err(format!("expected a refusal, opened {tree:?}").into()),
    }
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_file_ignored_only_by_the_global_excludes_file_is_not_snapshotted() -> Result<(), Box<dyn Error>>
{
    let clone = Clone::new()?;
    set_excludes_file(&clone, "~/global-ignore")?;
    fs::write(clone.home().join("global-ignore"), ".env.local\n.idea/\n")?;
    write(&clone.root, ".env.local", b"SECRET=1\n")?;
    write(&clone.root, ".idea/workspace.xml", b"<x/>\n")?;
    write(&clone.root, "src/new.rs", b"pub fn new() {}\n")?;

    let paths = snapshot_paths(&clone)?;

    assert_eq!(
        paths,
        [".gitignore", "src/gone.rs", "src/lib.rs", "src/new.rs"],
        "what the developer ignores everywhere never reaches the model"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_default_excludes_file_under_xdg_config_home_is_honoured() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let xdg = clone.home().join("xdg");
    write(&xdg, "git/ignore", b"*.local\n")?;
    write(&clone.root, "notes.local", b"private\n")?;
    let user = UserDirs::new(
        Some(clone.home().into_os_string()),
        Some(xdg.into_os_string()),
    );
    let tree = WorkingTree::open(
        &clone.root,
        GitEnvironment::from_process_env()?,
        &user,
        GitLimits::default(),
    )?;
    let repo = clone.scratch_repo("snap.git")?;

    let snapshot = repo.snapshot(&tree, &clone.head)?;

    assert!(
        files(&repo, &snapshot)?
            .iter()
            .all(|(path, _)| path != "notes.local"),
        "git's default excludes file applies when core.excludesFile is unset"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_repository_exclude_file_overrides_the_global_one() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    set_excludes_file(&clone, "~/global-ignore")?;
    fs::write(clone.home().join("global-ignore"), "*.txt\n")?;
    fs::write(clone.root.join(".git/info/exclude"), "!keep.txt\n")?;
    write(&clone.root, "keep.txt", b"kept\n")?;
    write(&clone.root, "drop.txt", b"dropped\n")?;

    let paths = snapshot_paths(&clone)?;

    assert!(paths.iter().any(|path| path == "keep.txt"), "{paths:?}");
    assert!(paths.iter().all(|path| path != "drop.txt"), "{paths:?}");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_relative_global_excludes_file_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    set_excludes_file(&clone, "relative-ignore")?;

    assert!(
        refusal(&clone)?.ends_with("its core.excludesFile is not an absolute path"),
        "git would read it relative to wherever it runs"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_unreadable_global_excludes_file_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    set_excludes_file(&clone, "~/ignore-dir")?;
    fs::create_dir(clone.home().join("ignore-dir"))?;
    let tree = clone.open()?;
    let repo = clone.scratch_repo("snap.git")?;

    let result = repo.snapshot(&tree, &clone.head);

    assert!(
        matches!(&result, Err(GitError::Inspect { .. })),
        "rules that cannot be read are not skipped, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_fifo_global_excludes_file_is_refused_without_blocking() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    set_excludes_file(&clone, "~/ignore-fifo")?;
    let fifo = clone.home().join("ignore-fifo");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success(), "mkfifo {}", fifo.display());
    let tree = clone.open()?;
    let repo = clone.scratch_repo("snap.git")?;

    let result = repo.snapshot(&tree, &clone.head);

    assert!(
        matches!(&result, Err(GitError::Inspect { git_dir, .. }) if *git_dir == fifo),
        "a FIFO is refused, never opened and waited on, got {result:?}"
    );
    Ok(())
}

/// A comment line of `len` bytes, newline included.
fn exclude_text(len: usize) -> String {
    let mut text = "#".repeat(len.saturating_sub(1));
    text.push('\n');
    text
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn info_exclude_at_one_mib_snapshots_and_one_byte_more_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let exclude = clone.root.join(".git/info/exclude");
    fs::write(&exclude, exclude_text(1 << 20))?;
    let at_cap = clone.snapshot("at-cap.git").map(|_snapshot| ());
    fs::write(&exclude, exclude_text((1 << 20) + 1))?;
    let tree = clone.open()?;
    let over = clone.scratch_repo("over.git")?.snapshot(&tree, &clone.head);

    assert!(at_cap.is_ok(), "{at_cap:?}");
    assert!(
        matches!(&over, Err(GitError::NotAWorkingTree { reason, .. })
            if *reason == "its exclude files are over 1 MiB together"),
        "{over:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_global_and_repository_exclude_files_share_the_cap() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    set_excludes_file(&clone, "~/global-ignore")?;
    fs::write(clone.home().join("global-ignore"), exclude_text(1 << 19))?;
    fs::write(
        clone.root.join(".git/info/exclude"),
        exclude_text((1 << 19) + 1),
    )?;
    let tree = clone.open()?;

    let over = clone.scratch_repo("snap.git")?.snapshot(&tree, &clone.head);

    assert!(
        matches!(&over, Err(GitError::NotAWorkingTree { .. })),
        "{over:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_sparse_checkout_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(
        &clone.root,
        &["sparse-checkout", "set", "--no-cone", "/src/lib.rs"],
    )?;

    assert!(
        refusal(&clone)?.ends_with("core.sparseCheckout is set"),
        "files outside the cone would read as deleted"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_disabled_sparse_checkout_snapshots_every_file() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(
        &clone.root,
        &["sparse-checkout", "set", "--no-cone", "/src/lib.rs"],
    )?;
    plain_git(&clone.root, &["sparse-checkout", "disable"])?;

    let paths = snapshot_paths(&clone)?;

    assert!(
        clone.root.join(".git/info/sparse-checkout").exists(),
        "disable leaves the file behind"
    );
    assert_eq!(paths, [".gitignore", "src/gone.rs", "src/lib.rs"]);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn core_sparse_checkout_alone_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(&clone.root, &["config", "core.sparseCheckout", "true"])?;

    assert!(refusal(&clone)?.ends_with("core.sparseCheckout is set"));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_skip_worktree_entry_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(
        &clone.root,
        &["update-index", "--skip-worktree", "src/gone.rs"],
    )?;

    assert!(refusal(&clone)?.ends_with("its index has skip-worktree entries"));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "touches the file system")]
fn a_dot_git_symlink_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let linked = clone.scratch.path().join("linked");
    fs::create_dir(&linked)?;
    std::os::unix::fs::symlink(clone.root.join(".git"), linked.join(".git"))?;

    assert_eq!(
        open_error(&linked)?,
        "its .git is neither a directory nor a file"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "touches the file system")]
fn a_gitdir_holding_a_nul_is_refused() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    fs::write(scratch.path().join(".git"), b"gitdir: /repo\0.git\n")?;

    assert_eq!(
        open_error(scratch.path())?,
        "its .git file is not one `gitdir: <path>` line"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn checkout_refuses_a_destination_it_cannot_read_as_a_directory() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let (repo, snapshot) = clone.snapshot("snap.git")?;
    let dest = clone.scratch.path().join("a-file");
    fs::write(&dest, "not a directory\n")?;

    let result = repo.checkout(&snapshot, &dest);

    assert!(
        matches!(&result, Err(GitError::Inspect { git_dir, .. }) if git_dir == Path::new(&dest)),
        "{result:?}"
    );
    Ok(())
}
