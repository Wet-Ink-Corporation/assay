//! The repository layout of `docs/architecture/02-architecture.md` §7. Directories that hold no
//! content yet carry a README saying what will live there.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const CRATES: [&str; 6] = [
    "assay-domain",
    "assay-app",
    "assay-forge",
    "assay-runner",
    "assay-gates",
    "assay-cli",
];

/// Directories from 02 §7 that exist before their content does, each documented by a README.
const PLACEHOLDER_DIRS: [&str; 11] = [
    "plugin",
    "plugin/commands",
    "plugin/prompts",
    "plugin/hooks",
    "workflows",
    "rubric",
    "schemas",
    "corpus/dev",
    "corpus/test",
    "corpus/inject",
    "docs/decisions",
];

const ROOT_FILES: [&str; 8] = [
    "AGENTS.md",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "deny.toml",
    ".github/workflows/rust.yml",
    ".claude/temper.json",
    ".claude/settings.json",
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn missing(root: &Path, relative: impl IntoIterator<Item = String>) -> Vec<String> {
    relative
        .into_iter()
        .filter(|path| !root.join(path).is_file())
        .collect()
}

#[test]
fn every_section_7_file_exists() {
    let root = repository_root();
    let charters = CRATES.iter().flat_map(|name| {
        [
            format!("crates/{name}/AGENTS.md"),
            format!("crates/{name}/CLAUDE.md"),
        ]
    });
    let expected = ROOT_FILES
        .iter()
        .map(|path| (*path).to_owned())
        .chain(charters);

    assert_eq!(
        missing(&root, expected),
        Vec::<String>::new(),
        "02 §7 files are missing"
    );
}

#[test]
fn every_placeholder_directory_carries_a_readme() {
    let root = repository_root();
    let readmes = PLACEHOLDER_DIRS
        .iter()
        .map(|dir| format!("{dir}/README.md"));

    assert_eq!(
        missing(&root, readmes),
        Vec::<String>::new(),
        "02 §7 directories lack a README"
    );
}

fn level_two_headings(text: &str) -> BTreeSet<&str> {
    text.lines()
        .filter(|line| line.starts_with("## "))
        .collect()
}

#[test]
fn root_claude_md_imports_the_charter_without_duplicating_it() -> Result<(), std::io::Error> {
    let root = repository_root();
    let charter = fs::read_to_string(root.join("AGENTS.md"))?;
    let claude = fs::read_to_string(root.join("CLAUDE.md"))?;

    assert_eq!(
        claude.lines().next(),
        Some("@AGENTS.md"),
        "CLAUDE.md must import AGENTS.md"
    );
    assert_eq!(
        level_two_headings(&charter)
            .intersection(&level_two_headings(&claude))
            .collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "CLAUDE.md repeats sections of AGENTS.md (02 §2)"
    );
    Ok(())
}
