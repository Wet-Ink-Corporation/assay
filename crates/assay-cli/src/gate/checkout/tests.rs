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

/// [`walk`] of `checkout` well within the entry cap; its error as text.
fn walked(checkout: &Path, tree: &[TreeEntry]) -> Result<BTreeMap<String, OnDisk>, Box<dyn Error>> {
    walk(checkout, tree, 100).map_err(|error| error.to_string().into())
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
fn the_walk_stops_past_the_entry_cap() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    for name in ["a", "b", "c"] {
        fs::write(dir.path().join(name), "z")?;
    }

    let at_cap = walk(dir.path(), &[], 3).map(|found| found.len());
    let past = walk(dir.path(), &[], 2);

    assert!(matches!(at_cap, Ok(3)), "{at_cap:?}");
    assert!(
        matches!(past, Err(PinnedError::CheckoutTooLarge { max: 2 })),
        "{past:?}"
    );
    Ok(())
}
