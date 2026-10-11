//! No git call path bypasses the hardened constructor: production code creates a process in
//! exactly one place, `src/git/command.rs` (`docs/architecture/03-contracts.md` §9).

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(rust_files(&path)?);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(files)
}

/// The text of `source` before its first `#[cfg(test)]` item: the production code, given the
/// crate's convention of test modules at the bottom of a file.
fn production_part(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .map_or(source, |(production, _tests)| production)
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; Miri isolates the file system"
)]
fn only_the_hardened_constructor_creates_processes() -> Result<(), Box<dyn Error>> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut constructors = Vec::new();

    for file in rust_files(&src)? {
        let source = fs::read_to_string(&file)?;
        let production = production_part(&source);
        for pattern in ["Command::new", "process::Command::"] {
            let hits = production.matches(pattern).count();
            let relative = file.strip_prefix(&src)?.to_string_lossy().into_owned();
            constructors.extend(std::iter::repeat_n(relative, hits));
        }
    }

    assert_eq!(
        constructors,
        [Path::new("git").join("command.rs").to_string_lossy()],
        "every process must be created by the hardened git constructor"
    );
    Ok(())
}
