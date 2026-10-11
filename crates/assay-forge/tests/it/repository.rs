//! The object-only repository Z4 fetches PR objects into.

use std::error::Error;

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
