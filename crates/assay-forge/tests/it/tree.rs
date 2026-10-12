//! Reading files of a commit's tree as data: `ObjectRepo::read_tree` (03 §2.1 steps 1-3 read
//! `.claude/temper-baseline.json`, `.claude/temper.json` and `.assay/exceptions.toml` this way).

use std::error::Error;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use assay_forge::git::{TreeContent, TreeFile};

use crate::support::{fixture, fixture_with_head, write};

/// Each file as its path and content, for whole-value comparisons.
fn entries(files: &[TreeFile]) -> Vec<(&str, &TreeContent)> {
    files
        .iter()
        .map(|file| (file.path(), file.content()))
        .collect()
}

fn bytes(text: &str) -> TreeContent {
    TreeContent::File(text.as_bytes().to_vec())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn reads_the_bytes_of_one_file_at_each_commit() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[("src/lib.rs", b"pub fn two() -> u8 {\n    2\n}\n")])?;

    let base = fixture.repo.read_tree(&fixture.base, "src/lib.rs")?;
    let head = fixture.repo.read_tree(&fixture.head, "src/lib.rs")?;

    assert_eq!(
        (entries(&base), entries(&head)),
        (
            vec![("src/lib.rs", &bytes("pub fn one() -> u8 {\n    1\n}\n"))],
            vec![("src/lib.rs", &bytes("pub fn two() -> u8 {\n    2\n}\n"))],
        ),
        "each commit's own blob is read"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn lists_every_file_under_a_directory_and_nothing_beside_it() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[
        (".claude/temper/temper.mjs", b"main\n"),
        (".claude/temper/lib/scan.mjs", b"scan\n"),
        (".claude/temper-baseline.json", b"{}\n"),
        (".claude/temper.json", b"{}\n"),
    ])?;

    let files = fixture.repo.read_tree(&fixture.head, ".claude/temper")?;

    assert_eq!(
        entries(&files),
        vec![
            (".claude/temper/lib/scan.mjs", &bytes("scan\n")),
            (".claude/temper/temper.mjs", &bytes("main\n")),
        ],
        "a directory is read recursively, and a sibling sharing its prefix is not under it"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_absent_path_has_no_files() -> Result<(), Box<dyn Error>> {
    let fixture = fixture(&[(".claude/temper.json", b"{}\n")])?;

    let files = fixture
        .repo
        .read_tree(&fixture.base, ".claude/temper.json")?;

    assert_eq!(entries(&files), Vec::new(), "the file exists only at head");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn symlinks_and_executables_are_told_apart_from_plain_files() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_with_head(|work| {
        write(work, "tools/run.sh", b"#!/bin/sh\n")?;
        let script = work.join("tools/run.sh");
        let mut permissions = fs::metadata(&script)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions)?;
        symlink("../README.md", work.join("tools/readme"))?;
        Ok(())
    })?;

    let files = fixture.repo.read_tree(&fixture.head, "tools")?;

    assert_eq!(
        entries(&files),
        vec![
            ("tools/readme", &TreeContent::Symlink),
            (
                "tools/run.sh",
                &TreeContent::Executable(b"#!/bin/sh\n".to_vec())
            ),
        ],
        "a symlink is never read through, and an executable keeps its mode"
    );
    Ok(())
}
