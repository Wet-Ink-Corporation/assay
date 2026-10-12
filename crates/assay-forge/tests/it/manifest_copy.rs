//! The sanitised manifest-only copy of a commit (03 §2.1, 04 §2.2): what the pinned base
//! cargo reads to compute the package set, holding no PR content but manifests and no PR
//! configuration at all.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::Path;

use assay_forge::git::{CopyError, TreeLimits};

use crate::support::{Fixture, fixture, fixture_with_head, plain_git, write};

/// The copy of `fixture`'s head, into a fresh scratch directory.
fn copy_head(fixture: &Fixture) -> Result<Result<(), CopyError>, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    Ok(fixture
        .repo
        .manifest_copy(
            &fixture.head,
            &scratch.path().join("copy"),
            TreeLimits::default(),
        )
        .map(drop))
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlink_to_a_directory_is_refused() -> Result<(), Box<dyn Error>> {
    // #65 review F2: as an empty file it would hide a directory cargo's glob `members` (or a
    // path dependency) sees in the real tree.
    let fixture = fixture_with_head(|work| {
        write(
            work,
            "crates/x/Cargo.toml",
            "[package]\nname = \"x\"\n".as_bytes(),
        )?;
        symlink("x", work.join("crates/y"))?;
        Ok(())
    })?;

    let result = copy_head(&fixture)?;

    assert!(
        matches!(&result, Err(CopyError::Symlink { path }) if path == "crates/y"),
        "got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlink_leaving_the_tree_is_refused() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        fs::create_dir_all(work.join("crates/x"))?;
        symlink("../../../outside/build.rs", work.join("crates/x/build.rs"))?;
        Ok(())
    })?;

    let result = copy_head(&fixture)?;

    assert!(
        matches!(&result, Err(CopyError::Symlink { path }) if path == "crates/x/build.rs"),
        "got {result:?}"
    );
    Ok(())
}

/// The result of copying a head that adds the symlink `link` with the raw target `target`.
fn copy_with_link(link: &str, target: &[u8]) -> Result<Result<(), CopyError>, Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        let link = work.join(link);
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }
        symlink(OsStr::from_bytes(target), link)?;
        Ok(())
    })?;
    copy_head(&fixture)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlink_to_an_absolute_path_is_refused() -> Result<(), Box<dyn Error>> {
    let result = copy_with_link("crates/x/build.rs", b"/etc/passwd")?;

    assert!(
        matches!(&result, Err(CopyError::Symlink { path }) if path == "crates/x/build.rs"),
        "got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlink_whose_target_is_not_utf8_is_refused() -> Result<(), Box<dyn Error>> {
    let result = copy_with_link("crates/x/build.rs", b"gen\xff.rs")?;

    assert!(
        matches!(&result, Err(CopyError::Symlink { path }) if path == "crates/x/build.rs"),
        "got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlink_to_its_own_directory_is_refused() -> Result<(), Box<dyn Error>> {
    let result = copy_with_link("crates/here", b".")?;

    assert!(
        matches!(&result, Err(CopyError::Symlink { path }) if path == "crates/here"),
        "got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlink_climbing_back_to_the_root_is_refused() -> Result<(), Box<dyn Error>> {
    let result = copy_with_link("back", b"sub/..")?;

    assert!(
        matches!(&result, Err(CopyError::Symlink { path }) if path == "back"),
        "got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlink_to_a_symlink_is_refused() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        symlink("README.md", work.join("first"))?;
        symlink("first", work.join("second"))?;
        Ok(())
    })?;

    let result = copy_head(&fixture)?;

    assert!(
        matches!(&result, Err(CopyError::Symlink { path }) if path == "second"),
        "got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_submodule_is_refused() -> Result<(), Box<dyn Error>> {
    // A submodule's crates exist in the real build but not in the copy.
    let fixture = fixture_with_head(|work| {
        // An unpopulated submodule's directory, so `git add --all` keeps the gitlink.
        fs::create_dir_all(work.join("vendor/sub"))?;
        plain_git(
            work,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0123456789abcdef0123456789abcdef01234567,vendor/sub",
            ],
        )?;
        Ok(())
    })?;

    let result = copy_head(&fixture)?;

    assert!(
        matches!(&result, Err(CopyError::Submodule { path }) if path == "vendor/sub"),
        "got {result:?}"
    );
    Ok(())
}

/// Files by path relative to a copy's root, with their contents.
type Files = Vec<(String, Vec<u8>)>;

/// Every regular file under `root`, relative to it, with its contents, sorted.
fn files(root: &Path) -> Result<Files, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root)?.to_string_lossy().into_owned();
                found.push((relative, fs::read(&path)?));
            }
        }
    }
    found.sort();
    Ok(found)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn manifests_are_copied_verbatim_and_other_files_are_empty() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        ("Cargo.lock", b"version = 4\n"),
        ("crates/x/Cargo.toml", b"[package]\nname = \"x\"\n"),
        ("crates/x/build.rs", b"fn main() { evil() }\n"),
    ])?;
    let scratch = tempfile::tempdir()?;

    let copy = fixture.repo.manifest_copy(
        &fixture.head,
        &scratch.path().join("copy"),
        TreeLimits::default(),
    )?;

    assert_eq!(
        files(copy.root())?,
        [
            ("Cargo.lock".to_owned(), b"version = 4\n".to_vec()),
            (
                "Cargo.toml".to_owned(),
                b"[package]\nname = \"fixture\"\n".to_vec()
            ),
            ("README.md".to_owned(), Vec::new()),
            (
                "crates/x/Cargo.toml".to_owned(),
                b"[package]\nname = \"x\"\n".to_vec()
            ),
            ("crates/x/build.rs".to_owned(), Vec::new()),
            ("legacy/kept.rs".to_owned(), Vec::new()),
            ("src/lib.rs".to_owned(), Vec::new()),
        ],
        "cargo finds every target, but reads no PR content except manifests"
    );
    assert_eq!(
        copy.manifests(),
        ["Cargo.lock", "Cargo.toml", "crates/x/Cargo.toml"],
        "the manifests written, in git's order"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn toolchain_files_cargo_config_and_attributes_are_never_copied() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        (
            ".cargo/config.toml",
            b"[build]\nrustc-wrapper = \"./evil\"\n",
        ),
        ("crates/x/.cargo/config", b"[build]\nrustc = \"./evil\"\n"),
        (".CARGO/config.toml", b"[build]\nrustc = \"./evil\"\n"),
        ("rust-toolchain.toml", b"[toolchain]\nchannel = \"evil\"\n"),
        ("crates/x/rust-toolchain", b"evil\n"),
        (".gitattributes", b"* -diff\n"),
        ("crates/x/.gitattributes", b"* -diff\n"),
    ])?;
    let scratch = tempfile::tempdir()?;

    let copy = fixture.repo.manifest_copy(
        &fixture.head,
        &scratch.path().join("copy"),
        TreeLimits::default(),
    )?;

    let names: Vec<String> = files(copy.root())?
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        ["Cargo.toml", "README.md", "legacy/kept.rs", "src/lib.rs"],
        "no PR configuration reaches the pinned cargo"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_symlinked_manifest_is_an_empty_file() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        write(
            work,
            "crates/real/Cargo.toml",
            b"[package]\nname = \"real\"\n",
        )?;
        fs::create_dir_all(work.join("crates/x"))?;
        symlink("../real/Cargo.toml", work.join("crates/x/Cargo.toml"))?;
        Ok(())
    })?;
    let scratch = tempfile::tempdir()?;

    let copy = fixture.repo.manifest_copy(
        &fixture.head,
        &scratch.path().join("copy"),
        TreeLimits::default(),
    )?;

    let link = copy.root().join("crates/x/Cargo.toml");
    assert_eq!(
        (fs::symlink_metadata(&link)?.is_file(), fs::read(&link)?),
        (true, Vec::new()),
        "a symlink is never followed or recreated; as an empty manifest it fails cargo (closed)"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_directory_with_contents_is_refused() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;
    let scratch = tempfile::tempdir()?;
    fs::write(
        scratch.path().join("Cargo.toml"),
        "[package]\nname = \"stale\"\n",
    )?;

    let result = fixture
        .repo
        .manifest_copy(&fixture.head, scratch.path(), TreeLimits::default());

    assert!(
        matches!(result, Err(CopyError::DirectoryNotEmpty { .. })),
        "a stale manifest could be read as the PR's, got {result:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_tree_over_the_entry_limit_is_refused() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[])?;
    let scratch = tempfile::tempdir()?;
    let limits = TreeLimits {
        max_entries: 1,
        ..TreeLimits::default()
    };

    let result = fixture
        .repo
        .manifest_copy(&fixture.head, &scratch.path().join("copy"), limits);

    assert!(
        matches!(result, Err(CopyError::List(_))),
        "the copy is bounded by the listing, got {result:?}"
    );
    Ok(())
}
