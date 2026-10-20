//! What `assay status` reads of the developer's checkout besides its objects (#273): the
//! `origin` remote's URL, which names the repository, and the branch `HEAD` is on. Both are read
//! from the repository's own config and refs; anything but one plain answer is refused.

use std::error::Error;
use std::fs;

use assay_forge::git::{GitError, GitOperation};

use super::Clone;
use crate::support::plain_git;

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_origin_url_is_read_from_the_repository_config() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(
        &clone.root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    )?;
    let tree = clone.open()?;

    let url = tree.origin_url()?;

    assert_eq!(url.as_deref(), Some("https://github.com/acme/widgets.git"));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn no_origin_is_none() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    let tree = clone.open()?;

    let url = tree.origin_url()?;

    assert_eq!(url, None);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_origin_with_two_urls_is_refused() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(
        &clone.root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    )?;
    plain_git(
        &clone.root,
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://github.com/acme/other.git",
        ],
    )?;
    let tree = clone.open()?;

    let url = tree.origin_url();

    assert!(
        matches!(
            url,
            Err(GitError::Malformed {
                operation: GitOperation::ConfigValue,
                ..
            })
        ),
        "{url:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_user_config_does_not_supply_an_origin() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    fs::write(
        clone.home().join(".gitconfig"),
        "[remote \"origin\"]\n\turl = https://github.com/elsewhere/repo.git\n",
    )?;
    let tree = clone.open()?;

    let url = tree.origin_url()?;

    assert_eq!(
        url, None,
        "only the repository's own config names its origin"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_branch_head_is_on_is_named() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(
        &clone.root,
        &["checkout", "--quiet", "-b", "feature/status"],
    )?;
    let tree = clone.open()?;

    let branch = tree.branch()?;

    assert_eq!(branch.as_deref(), Some("feature/status"));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_detached_head_is_on_no_branch() -> Result<(), Box<dyn Error>> {
    let clone = Clone::new()?;
    plain_git(
        &clone.root,
        &["checkout", "--quiet", "--detach", clone.head.as_str()],
    )?;
    let tree = clone.open()?;

    let branch = tree.branch()?;

    assert_eq!(branch, None);
    Ok(())
}
