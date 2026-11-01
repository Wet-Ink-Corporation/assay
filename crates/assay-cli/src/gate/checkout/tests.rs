//! The checkout's comparison with the tree, and its walk (unit, #348).

use std::error::Error;
use std::os::unix::fs::symlink;

use super::*;

fn entry(path: &str, kind: EntryKind, size: Option<u64>) -> TreeEntry {
    TreeEntry {
        path: path.to_owned(),
        kind,
        object: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        size,
    }
}

fn file(size: u64) -> OnDisk {
    OnDisk::File {
        executable: false,
        size,
    }
}

fn disk(found: &[(&str, OnDisk)]) -> BTreeMap<String, OnDisk> {
    found
        .iter()
        .map(|(path, on_disk)| ((*path).to_owned(), *on_disk))
        .collect()
}

fn differs(path: &str, difference: Difference) -> Result<Vec<String>, Mismatch> {
    Err(Mismatch {
        path: path.to_owned(),
        difference,
    })
}

/// Room for 100 files and 100 directories.
const ROOMY: Caps = Caps {
    files: 100,
    directories: 100,
};

/// [`walk`] of `checkout` well within the caps; its stop as text.
fn walked(checkout: &Path, tree: &[TreeEntry]) -> Result<BTreeMap<String, OnDisk>, Box<dyn Error>> {
    walk(checkout, tree, ROOMY).map_err(|stop| format!("{stop:?}").into())
}

/// Whether `walked` stopped at a cap of `caps`.
fn too_large(walked: &Result<BTreeMap<String, OnDisk>, Stop>, caps: Caps) -> bool {
    matches!(
        walked,
        Err(Stop::Refused(PinnedError::CheckoutTooLarge { files, directories }))
            if *files == caps.files && *directories == caps.directories
    )
}

/// [`compare`]'s verdict, with the paths it leaves to compare by bytes.
fn compared(tree: &[TreeEntry], found: &[(&str, OnDisk)]) -> Result<Vec<String>, Mismatch> {
    compare(tree, &disk(found)).map(|blobs| blobs.iter().map(|entry| entry.path.clone()).collect())
}

#[test]
fn a_matching_checkout_leaves_every_file_and_symlink_to_compare_by_bytes() {
    let tree = [
        entry("bin/run", EntryKind::Executable, Some(3)),
        entry("link", EntryKind::Symlink, Some(4)),
        entry("src/lib.rs", EntryKind::File, Some(5)),
        entry("vendor/sub", EntryKind::Submodule, None),
    ];
    let found = [
        (
            "bin/run",
            OnDisk::File {
                executable: true,
                size: 3,
            },
        ),
        ("link", OnDisk::Symlink),
        ("src/lib.rs", file(5)),
        ("vendor/sub", OnDisk::EmptyDir),
    ];

    assert_eq!(
        compared(&tree, &found),
        Ok(vec![
            "bin/run".to_owned(),
            "link".to_owned(),
            "src/lib.rs".to_owned()
        ])
    );
}

#[test]
fn a_tracked_entry_the_checkout_lacks_is_missing() {
    let tree = [entry("Cargo.toml", EntryKind::File, Some(1))];

    assert_eq!(
        compared(&tree, &[]),
        differs("Cargo.toml", Difference::Missing)
    );
}

#[test]
fn an_entry_the_tree_lacks_is_untracked() {
    let tree = [entry("Cargo.toml", EntryKind::File, Some(1))];
    let found = [("Cargo.toml", file(1)), (".cargo/config.toml", file(9))];

    assert_eq!(
        compared(&tree, &found),
        differs(".cargo/config.toml", Difference::Untracked)
    );
}

#[test]
fn another_executable_bit_is_a_mode_difference_either_way() {
    let made_executable = compared(
        &[entry("a", EntryKind::File, Some(1))],
        &[(
            "a",
            OnDisk::File {
                executable: true,
                size: 1,
            },
        )],
    );
    let made_plain = compared(
        &[entry("a", EntryKind::Executable, Some(1))],
        &[("a", file(1))],
    );

    assert_eq!(made_executable, differs("a", Difference::Mode));
    assert_eq!(made_plain, differs("a", Difference::Mode));
}

#[test]
fn another_size_is_other_bytes() {
    let tree = [entry("a", EntryKind::File, Some(1))];

    assert_eq!(
        compared(&tree, &[("a", file(2))]),
        differs("a", Difference::Content)
    );
}

#[test]
fn another_kind_of_entry_is_refused() {
    let cases = [
        (entry("a", EntryKind::Symlink, Some(1)), file(1)),
        (entry("a", EntryKind::File, Some(1)), OnDisk::Symlink),
        (entry("a", EntryKind::File, Some(1)), OnDisk::Other),
        (entry("a", EntryKind::Submodule, None), OnDisk::Dir),
        (entry("a", EntryKind::Submodule, None), file(0)),
    ];

    for (tracked, on_disk) in cases {
        assert_eq!(
            compared(&[tracked], &[("a", on_disk)]),
            differs("a", Difference::Kind),
            "{on_disk:?}"
        );
    }
}

#[test]
fn the_walk_skips_the_root_git_dir_and_an_untracked_root_target() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    for path in [".git/HEAD", "target/debug/x", "src/target/y", "src/.git"] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().ok_or("a parent")?)?;
        fs::write(path, "z")?;
    }
    symlink("../elsewhere", root.join("src/link"))?;

    let found = walked(root, &[])?;

    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        ["src/.git", "src/link", "src/target/y"],
        "only the root's `.git` and untracked `target` are not the checkout's content"
    );
    assert_eq!(found.get("src/link"), Some(&OnDisk::Symlink));
    Ok(())
}

#[test]
fn a_tracked_target_is_walked_like_any_directory() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    fs::create_dir_all(dir.path().join("target"))?;
    fs::write(dir.path().join("target/kept"), "z")?;
    fs::write(dir.path().join("target/restored"), "z")?;
    let tree = [entry("target/kept", EntryKind::File, Some(1))];

    let found = walked(dir.path(), &tree)?;

    assert_eq!(
        compare(&tree, &found).map(|blobs| blobs.len()),
        Err(Mismatch {
            path: "target/restored".to_owned(),
            difference: Difference::Untracked
        })
    );
    Ok(())
}

#[test]
fn a_submodule_directory_is_recorded_as_empty_or_not_and_never_entered()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    fs::create_dir_all(dir.path().join("empty"))?;
    fs::create_dir_all(dir.path().join("full"))?;
    fs::write(dir.path().join("full/inside"), "z")?;
    let tree = [
        entry("empty", EntryKind::Submodule, None),
        entry("full", EntryKind::Submodule, None),
    ];

    let found = walked(dir.path(), &tree)?;

    assert_eq!(
        found,
        disk(&[("empty", OnDisk::EmptyDir), ("full", OnDisk::Dir)])
    );
    Ok(())
}

#[test]
fn the_walk_stops_past_the_file_cap_inside_one_large_directory() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    for name in 0..10 {
        fs::write(dir.path().join(name.to_string()), "z")?;
    }
    let at_cap = Caps {
        files: 10,
        directories: 0,
    };
    let below = Caps {
        files: 3,
        directories: 0,
    };

    let whole = walk(dir.path(), &[], at_cap).map(|found| found.len());
    let past = walk(dir.path(), &[], below);

    assert!(matches!(whole, Ok(10)), "{whole:?}");
    assert!(too_large(&past, below), "{past:?}");
    Ok(())
}

#[test]
fn the_walk_stops_past_the_directory_cap() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    fs::create_dir_all(dir.path().join("a/b/c"))?;
    let at_cap = Caps {
        files: 0,
        directories: 3,
    };
    let below = Caps {
        files: 0,
        directories: 2,
    };

    let whole = walk(dir.path(), &[], at_cap).map(|found| found.len());
    let past = walk(dir.path(), &[], below);

    assert!(
        matches!(whole, Ok(0)),
        "empty directories are no content: {whole:?}"
    );
    assert!(too_large(&past, below), "{past:?}");
    Ok(())
}

#[test]
fn the_caps_admit_the_listing_cap_of_files_and_every_directory_the_tree_implies() {
    let tree = [
        entry("a/b/c/d/file", EntryKind::File, Some(1)),
        entry("a/other", EntryKind::File, Some(1)),
        entry("vendor/sub", EntryKind::Submodule, None),
    ];

    assert_eq!(
        Caps::for_tree(&tree, 7),
        Caps {
            files: 7,
            directories: 5 + UNTRACKED_DIRECTORIES
        },
        "a, a/b, a/b/c, a/b/c/d and vendor, plus the untracked allowance"
    );
}

#[test]
fn a_deep_tree_within_the_listing_cap_is_not_refused() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    fs::create_dir_all(dir.path().join("a/b/c/d"))?;
    fs::write(dir.path().join("a/b/c/d/file"), "z")?;
    let tree = [entry("a/b/c/d/file", EntryKind::File, Some(1))];

    let found = walk(dir.path(), &tree, Caps::for_tree(&tree, 1)).map(|found| found.len());

    assert!(
        matches!(found, Ok(1)),
        "directories are not counted as entries: {found:?}"
    );
    Ok(())
}

#[test]
fn a_name_that_is_not_utf8_is_untracked_even_beside_its_lossy_twin() -> Result<(), Box<dyn Error>> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("a\u{FFFD}"), "z")?;
    fs::write(dir.path().join(OsStr::from_bytes(b"a\xff")), "z")?;
    let tree = [entry("a\u{FFFD}", EntryKind::File, Some(1))];

    let walked = walk(dir.path(), &tree, ROOMY);

    assert!(
        matches!(
            &walked,
            Err(Stop::Differs(Mismatch { path, difference: Difference::Untracked }))
                if path == "a\u{FFFD}"
        ),
        "{walked:?}"
    );
    Ok(())
}

/// Runs git in `cwd`, isolated from the machine's config. Test setup only.
fn git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(format!("git {args:?}: {}", output.status).into())
    }
}

#[test]
fn bytes_are_compared_in_batches_and_a_difference_in_the_last_batch_is_found()
-> Result<(), Box<dyn Error>> {
    use assay_forge::git::{GitEnvironment, GitLimits};

    let dir = tempfile::tempdir()?;
    let work = dir.path().join("work");
    git(dir.path(), &["init", "--quiet", "work"])?;
    for name in ["a", "b", "c"] {
        fs::write(work.join(name), "four")?;
    }
    git(&work, &["add", "--all"])?;
    git(&work, &["commit", "--quiet", "-m", "head"])?;
    let objects = dir.path().join("objects.git");
    let repo = ObjectRepo::init(
        &objects,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = objects.to_str().ok_or("a UTF-8 path")?;
    git(
        &work,
        &["push", "--quiet", destination, "HEAD:refs/heads/pr"],
    )?;
    let head: CommitSha = git(&work, &["rev-parse", "HEAD"])?.trim().parse()?;
    // Two four-byte blobs per batch: `c` is read in a second batch.
    let limits = TreeLimits {
        max_entries: 100,
        max_total_bytes: 8,
    };
    let tree = repo.list_tree(&head, None, limits)?;
    let blobs: Vec<&TreeEntry> = tree.iter().collect();

    let clean = same_bytes(&repo, &work, &blobs, limits).map_err(|e| e.to_string())?;
    fs::write(work.join("c"), "FOUR")?;
    let changed = same_bytes(&repo, &work, &blobs, limits).map_err(|e| e.to_string())?;

    assert_eq!(clean, None);
    assert_eq!(
        changed,
        Some(Mismatch {
            path: "c".to_owned(),
            difference: Difference::Content
        })
    );
    Ok(())
}
