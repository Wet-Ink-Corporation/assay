//! A cap on what a child writes into one directory, checked on every poll of
//! [`run_capped`](super::run_capped): the child's group is killed once the tree holds more bytes or entries
//! than allowed, so a process that unpacks untrusted data (gh extracting an artifact) cannot
//! fill the disk (H-13).
//!
//! The tree is walked without following symlinks, at most `max_entries + 1` entries per walk,
//! so one check costs a bounded number of `stat` calls. What the child writes between two
//! checks is not seen until the next one: the overshoot is bounded by the poll interval
//! (5 ms) times the child's write rate, plus what it writes while its group is being killed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The most a child may write under one directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryCap<'a> {
    /// The directory watched, which the caller owns.
    pub dir: &'a Path,
    /// The most bytes its regular files may hold together.
    pub max_bytes: u64,
    /// The most entries (files, directories, links) it may hold at any depth.
    pub max_entries: usize,
}

impl DirectoryCap<'_> {
    /// Whether the tree under [`dir`](Self::dir) holds more than the cap allows. An entry that
    /// vanishes during the walk (a file the child renamed) is skipped.
    ///
    /// # Errors
    ///
    /// The I/O error of a listing or `stat` that fails other than with `NotFound`.
    pub(super) fn exceeded(&self) -> io::Result<bool> {
        let mut bytes: u64 = 0;
        let mut entries: usize = 0;
        let mut pending: Vec<PathBuf> = vec![self.dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Some(listing) = vanishing(fs::read_dir(&dir))? else {
                continue;
            };
            for entry in listing {
                let Some(entry) = vanishing(entry)? else {
                    continue;
                };
                entries = entries.saturating_add(1);
                if entries > self.max_entries {
                    return Ok(true);
                }
                // The entry's own metadata: a symlink is counted, never followed.
                let Some(metadata) = vanishing(entry.metadata())? else {
                    continue;
                };
                if metadata.is_dir() {
                    pending.push(entry.path());
                } else if metadata.is_file() {
                    bytes = bytes.saturating_add(metadata.len());
                    if bytes > self.max_bytes {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

/// `result`'s value, `None` when the entry is gone, or its other error.
fn vanishing<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn cap(dir: &Path, max_bytes: u64, max_entries: usize) -> DirectoryCap<'_> {
        DirectoryCap {
            dir,
            max_bytes,
            max_entries,
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads the file system; Miri isolates it")]
    fn bytes_at_any_depth_count_up_to_the_cap() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        fs::create_dir(dir.path().join("sub")).expect("a subdirectory");
        fs::write(dir.path().join("a"), b"12").expect("a file");
        fs::write(dir.path().join("sub/b"), b"345").expect("a nested file");

        let at = cap(dir.path(), 5, 8).exceeded().expect("the tree is read");
        let over = cap(dir.path(), 4, 8).exceeded().expect("the tree is read");

        assert_eq!((at, over), (false, true));
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads the file system; Miri isolates it")]
    fn entries_count_up_to_the_cap() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        fs::create_dir(dir.path().join("sub")).expect("a subdirectory");
        fs::write(dir.path().join("sub/b"), b"").expect("a nested file");

        let at = cap(dir.path(), 0, 2).exceeded().expect("the tree is read");
        let over = cap(dir.path(), 0, 1).exceeded().expect("the tree is read");

        assert_eq!((at, over), (false, true));
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads the file system; Miri isolates it")]
    fn a_symlink_is_counted_but_not_followed() {
        let outside = tempfile::tempdir().expect("a scratch directory");
        fs::write(outside.path().join("big"), [0_u8; 64]).expect("a file outside");
        let dir = tempfile::tempdir().expect("a scratch directory");
        symlink(outside.path().join("big"), dir.path().join("link")).expect("a symlink");
        symlink(outside.path(), dir.path().join("dir-link")).expect("a directory symlink");

        let exceeded = cap(dir.path(), 0, 2).exceeded().expect("the tree is read");

        assert!(!exceeded, "the link's target is never read");
    }
}
