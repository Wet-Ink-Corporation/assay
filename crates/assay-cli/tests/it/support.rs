//! Helpers shared by the integration test modules (TST-14).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Writes `text` as an executable (mode 0755) at `path`, replacing any file there.
///
/// A child `install` writes the executable, so no thread of this test binary ever holds it
/// open for writing: a sibling test's fork would inherit that descriptor until its exec, and
/// executing the file meanwhile fails with ETXTBSY (#176). Renaming alone would not help, as the
/// inode stays the same. The text goes to a `.source` file beside `path`, the copy to a
/// `.staged` one, renamed into place, so `path` is complete whenever it exists; both temporary
/// names are gone on success.
pub(crate) fn write_executable(path: &Path, text: &str) -> io::Result<()> {
    let source = beside(path, "source")?;
    let staged = beside(path, "staged")?;
    fs::write(&source, text)?;
    let installed = Command::new("install")
        .args(["-m", "0755"])
        .arg(&source)
        .arg(&staged)
        .status()?;
    if !installed.success() {
        return Err(io::Error::other(format!(
            "install {} failed: {installed}",
            staged.display()
        )));
    }
    fs::rename(&staged, path)?;
    fs::remove_file(&source)
}

/// `dir/.<name>.<suffix>` for `path` = `dir/name`.
fn beside(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} names no file", path.display())))?;
    let mut hidden = OsString::from(".");
    hidden.push(name);
    hidden.push(".");
    hidden.push(suffix);
    Ok(path.with_file_name(hidden))
}
