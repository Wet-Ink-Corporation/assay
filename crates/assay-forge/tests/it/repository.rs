//! The object-only repository Z4 fetches PR objects into.

use std::error::Error;

use assay_forge::git::{GitEnvironment, GitError, GitLimits, ObjectRepo};

use crate::support::{fixture, plain_git};

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_repository_is_bare_with_no_worktree() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;
    let git_dir = fixture.repo.git_dir();

    let bare = plain_git(git_dir, &["rev-parse", "--is-bare-repository"])?;

    assert_eq!(
        bare.trim(),
        "true",
        "Z4 must hold PR objects without a worktree"
    );
    assert!(
        !git_dir.join("index").exists(),
        "an object-only repository has no index"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn init_refuses_a_directory_that_already_has_contents() -> Result<(), Box<dyn Error>> {
    // An existing repository could carry config (core.sshCommand, aliases, filters) that the
    // hardened -c flags do not override.
    let scratch = tempfile::tempdir()?;
    std::fs::write(
        scratch.path().join("config"),
        "[core]\n\tfsmonitor = ./evil\n",
    )?;

    let result = ObjectRepo::init(
        scratch.path(),
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    );

    assert!(
        matches!(result, Err(GitError::DirectoryNotEmpty { .. })),
        "only an absent or empty directory may become the object repository, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn init_accepts_an_empty_directory() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;

    let repo = ObjectRepo::init(
        scratch.path(),
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;

    let bare = plain_git(repo.git_dir(), &["rev-parse", "--is-bare-repository"])?;
    assert_eq!(
        bare.trim(),
        "true",
        "the empty directory becomes a bare repository"
    );
    Ok(())
}
