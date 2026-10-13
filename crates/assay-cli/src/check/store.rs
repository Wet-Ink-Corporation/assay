//! Local mode's files in the git directory: each report as `<tree>.json`, and `latest`, the
//! tree of the last report written (#130), so `assay status --local` can show a stale result.
//!
//! Every file is written through a temporary file and a rename, so a reader never sees half
//! of one. Reads are bounded and take regular files only: a FIFO or device left at a report's
//! path is refused before it is opened, so it cannot block the read (#219 R1). A file swapped
//! for one between that check and the open is a residual: closing it needs `O_NONBLOCK`,
//! that is `libc` (H-16).

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use assay_app::{MAX_DOCUMENT_BYTES, is_tree_id};
use tempfile::NamedTempFile;

/// The file naming the tree of the last report written.
const LATEST: &str = "latest";

/// The most bytes of `latest` read: a SHA-256 tree id and a newline fit.
const MAX_LATEST_BYTES: usize = 128;

/// Why a report or `latest` could not be written.
#[derive(Debug)]
pub(crate) struct WriteError {
    pub(crate) path: PathBuf,
    pub(crate) source: io::Error,
}

/// Writes `json`, the report of `tree`, to `<dir>/<tree>.json`, then `tree` to `<dir>/latest`.
/// Returns the report's path.
pub(crate) fn write_report(dir: &Path, tree: &str, json: &[u8]) -> Result<PathBuf, WriteError> {
    let path = dir.join([tree, ".json"].concat());
    fs::create_dir_all(dir).map_err(|source| WriteError {
        // The error names the directory it could not create.
        path: dir.to_path_buf(),
        source,
    })?;
    replace(dir, &path, json)?;
    let latest = [tree, "\n"].concat();
    replace(dir, &dir.join(LATEST), latest.as_bytes())?;
    Ok(path)
}

/// Writes `bytes` to `path` in `dir` through a temporary file there.
fn replace(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let failed = |source| WriteError {
        // The error names the file it could not write.
        path: path.to_path_buf(),
        source,
    };
    let mut file = NamedTempFile::new_in(dir).map_err(failed)?;
    file.write_all(bytes).map_err(failed)?;
    file.persist(path).map_err(|error| failed(error.error))?;
    Ok(())
}

/// A report found for `assay status --local`.
#[derive(Debug)]
pub(crate) struct Found {
    /// The tree its file is named for.
    pub(crate) tree: String,
    /// Its bytes, at most one byte over [`MAX_DOCUMENT_BYTES`] (the app refuses that).
    pub(crate) bytes: Vec<u8>,
}

/// Why the local report could not be looked up.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// A file could not be read.
    Io { path: PathBuf, source: io::Error },
    /// A path holds something other than a regular file.
    NotAFile(PathBuf),
    /// `latest` does not hold one tree id and a newline.
    Latest(PathBuf),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::NotAFile(path) => write!(f, "{} is not a regular file", path.display()),
            Self::Latest(path) => write!(f, "{} does not name one tree", path.display()),
        }
    }
}

impl std::error::Error for ReadError {}

/// The report to show for the working tree whose fingerprint is `tree`: its own when there is
/// one, else the one `latest` names, else none.
pub(crate) fn find(dir: &Path, tree: &str) -> Result<Option<Found>, ReadError> {
    if let Some(found) = report(dir, tree)? {
        return Ok(Some(found));
    }
    let path = dir.join(LATEST);
    let Some(latest) = read_capped(&path, MAX_LATEST_BYTES)? else {
        return Ok(None);
    };
    let named = std::str::from_utf8(&latest)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .filter(|named| is_tree_id(named))
        .ok_or(ReadError::Latest(path))?;
    report(dir, named)
}

/// `<dir>/<tree>.json`, if it exists; `tree` must already read as a tree id.
fn report(dir: &Path, tree: &str) -> Result<Option<Found>, ReadError> {
    let path = dir.join([tree, ".json"].concat());
    Ok(read_capped(&path, MAX_DOCUMENT_BYTES)?.map(|bytes| Found {
        // The found report owns its tree's name.
        tree: tree.to_owned(),
        bytes,
    }))
}

/// At most `cap + 1` bytes of the regular file at `path`; `None` when there is nothing there.
fn read_capped(path: &Path, cap: usize) -> Result<Option<Vec<u8>>, ReadError> {
    let failed = |source| ReadError::Io {
        // The error names the file it could not read.
        path: path.to_path_buf(),
        source,
    };
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(ReadError::NotAFile(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed(error)),
    }
    let file = File::open(path).map_err(failed)?;
    if !file.metadata().map_err(failed)?.is_file() {
        return Err(ReadError::NotAFile(path.to_path_buf()));
    }
    let limit = u64::try_from(cap).map_or(u64::MAX, |cap| cap.saturating_add(1));
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(failed)?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;

    use super::{Found, MAX_LATEST_BYTES, ReadError, find, write_report};

    const TREE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const OTHER: &str = "dddddddddddddddddddddddddddddddddddddddd";

    fn found(found: Option<Found>) -> Option<(String, Vec<u8>)> {
        found.map(|found| (found.tree, found.bytes))
    }

    #[test]
    fn a_written_report_is_found_for_its_tree_and_through_latest_for_another()
    -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let assay = dir.path().join("assay");

        let path = write_report(&assay, TREE, b"{}\n").map_err(|error| error.source)?;

        assert_eq!(path, assay.join(format!("{TREE}.json")));
        assert_eq!(
            fs::read_to_string(assay.join("latest"))?,
            format!("{TREE}\n")
        );
        assert_eq!(
            found(find(&assay, TREE)?),
            Some((TREE.to_owned(), b"{}\n".to_vec()))
        );
        assert_eq!(
            found(find(&assay, OTHER)?),
            Some((TREE.to_owned(), b"{}\n".to_vec())),
            "another tree is shown the last report"
        );
        Ok(())
    }

    #[test]
    fn nothing_written_is_nothing_found() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;

        assert_eq!(found(find(&dir.path().join("assay"), TREE)?), None);
        Ok(())
    }

    #[test]
    fn latest_naming_a_report_that_is_gone_finds_nothing() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join("latest"), format!("{OTHER}\n"))?;

        assert_eq!(found(find(dir.path(), TREE)?), None);
        Ok(())
    }

    #[test]
    fn latest_that_does_not_name_one_tree_is_refused() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let over = format!("{}\n", "c".repeat(MAX_LATEST_BYTES));

        for text in ["../x\n", TREE, "", &format!("{TREE}\n\n"), over.as_str()] {
            fs::write(dir.path().join("latest"), text)?;
            let result = find(dir.path(), OTHER);
            assert!(
                matches!(result, Err(ReadError::Latest(_))),
                "{text:?}: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_directory_at_a_reports_path_is_refused() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        fs::create_dir(dir.path().join(format!("{TREE}.json")))?;

        let result = find(dir.path(), TREE);

        assert!(matches!(result, Err(ReadError::NotAFile(_))), "{result:?}");
        Ok(())
    }

    #[test]
    fn a_fifo_at_a_reports_path_is_refused_without_blocking() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let fifo = dir.path().join(format!("{TREE}.json"));
        let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
        assert!(made.success(), "mkfifo {}", fifo.display());

        let result = find(dir.path(), TREE);

        assert!(matches!(result, Err(ReadError::NotAFile(_))), "{result:?}");
        Ok(())
    }

    #[test]
    fn a_report_is_read_to_one_byte_past_the_document_cap() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let size = assay_app::MAX_DOCUMENT_BYTES + 10;
        fs::write(dir.path().join(format!("{TREE}.json")), vec![b' '; size])?;

        let read = find(dir.path(), TREE)?.map(|found| found.bytes.len());

        assert_eq!(read, Some(assay_app::MAX_DOCUMENT_BYTES + 1));
        Ok(())
    }
}
