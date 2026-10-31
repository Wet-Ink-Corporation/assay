//! Helpers shared by the integration test modules (TST-14).

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

/// Writes `text` as an executable (mode 0755) at `path`, replacing any file there.
///
/// A child `install` (one accepting `-m`, coreutils or BSD, found on `PATH`) writes the
/// executable, so no thread of this test binary ever holds it open for writing: a sibling
/// test's fork would inherit that descriptor until its exec, and executing the file meanwhile
/// fails with ETXTBSY (#176). Renaming alone would not help, as the inode stays the same. The
/// text and the copy are staged in a temporary directory beside `path`, and the copy is renamed
/// into place, so `path` is complete whenever it exists; the staging directory is removed on
/// every path, failures included (#249).
pub(crate) fn write_executable(path: &Path, text: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{} has no parent", path.display())))?;
    let staging = tempfile::tempdir_in(dir)?;
    let source = staging.path().join("source");
    let staged = staging.path().join("staged");
    fs::write(&source, text)?;
    install_with(Path::new("install"), &source, &staged, path)?;
    fs::rename(&staged, path)?;
    staging.close()
}

/// Copies `source` to `staged` with mode 0755 through the `install` at `program`; errors name
/// `program` and `target`, the executable the copy is for.
fn install_with(program: &Path, source: &Path, staged: &Path, target: &Path) -> io::Result<()> {
    let installed = Command::new(program)
        .args(["-m", "0755"])
        .arg(source)
        .arg(staged)
        .status()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "could not run {} (an install accepting -m on PATH) for {}: {error}",
                    program.display(),
                    target.display()
                ),
            )
        })?;
    if installed.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} failed for {}: {installed}",
            program.display(),
            target.display()
        )))
    }
}

mod tests {
    use std::fs;
    use std::io;
    use std::path::Path;

    use super::{install_with, write_executable};

    /// The names in `dir`, sorted.
    fn entries(dir: &Path) -> io::Result<Vec<String>> {
        let mut names = fs::read_dir(dir)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }

    #[test]
    fn writing_an_executable_leaves_only_the_executable() -> io::Result<()> {
        let dir = tempfile::tempdir()?;

        write_executable(&dir.path().join("tool"), "#!/bin/sh\n")?;

        assert_eq!(entries(dir.path())?, ["tool"], "no staging file is left");
        Ok(())
    }

    #[test]
    fn a_failed_move_into_place_leaves_no_staging_file() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        // A file cannot be renamed over a non-empty directory.
        fs::create_dir_all(dir.path().join("tool/inside"))?;

        let written = write_executable(&dir.path().join("tool"), "#!/bin/sh\n");

        assert_eq!(
            (written.is_err(), entries(dir.path())?),
            (true, vec!["tool".to_owned()]),
            "the move fails and the staging files are gone"
        );
        Ok(())
    }

    #[test]
    fn an_install_that_cannot_start_is_named_with_its_target() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("tool");

        let error = install_with(
            Path::new("/nonexistent/install"),
            &dir.path().join("source"),
            &dir.path().join("staged"),
            &target,
        )
        .expect_err("a missing install program cannot run");

        assert_eq!(
            (error.kind(), error.to_string()),
            (
                io::ErrorKind::NotFound,
                format!(
                    "could not run /nonexistent/install (an install accepting -m on PATH) for {}: \
                     No such file or directory (os error 2)",
                    target.display()
                )
            ),
            "the error names the program and the executable it was writing"
        );
        Ok(())
    }
}
