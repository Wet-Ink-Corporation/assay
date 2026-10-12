//! The PR's vendored temper (`.claude/temper/`) against the pinned copy (05 T4): assay never
//! runs the PR's copy, and reports its drift from the pin.
//!
//! The pinned copy is a directory in temper's vendored layout (`temper.mjs`, `lib/**`,
//! `VERSION`), prepared from the base's pin by the job. The PR's copy drifts when a file
//! differs in bytes, is missing, is extra, or is not a regular file. A PR with no vendored copy
//! has nothing to drift.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::revision::{FileContent, TreeFile};

/// The vendored copy's directory in a repository.
pub(super) const DIR: &str = ".claude/temper";

/// The pinned copy: each file's path relative to its root, `/`-separated, and its bytes.
pub(super) type Pinned = BTreeMap<String, Vec<u8>>;

/// Reads every file under `root`. A symlink, any other non-regular entry or a name that is not
/// UTF-8 is an error, as is an unreadable directory or file: the copy is trusted only whole.
pub(super) fn read_pinned(root: &Path) -> Result<Pinned, (PathBuf, io::Error)> {
    let mut files = Pinned::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| (directory.clone(), error))?;
        for entry in entries {
            let entry = entry.map_err(|error| (directory.clone(), error))?;
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return Err((path, invalid("the name is not UTF-8")));
            };
            let relative = format!("{prefix}{name}");
            let kind = entry.file_type().map_err(|error| (path.clone(), error))?;
            if kind.is_dir() {
                pending.push((path, format!("{relative}/")));
            } else if kind.is_file() {
                let bytes = fs::read(&path).map_err(|error| (path.clone(), error))?;
                files.insert(relative, bytes);
            } else {
                return Err((path, invalid("not a regular file or directory")));
            }
        }
    }
    Ok(files)
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// How one path of the PR's copy differs from the pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Drift {
    /// Its bytes differ from the pinned file's.
    Changed,
    /// The pin has it and the PR's copy does not.
    Missing,
    /// The PR's copy has it and the pin does not.
    Extra,
    /// It is a symlink or a submodule.
    NotRegular,
}

impl Drift {
    /// How the finding names this drift.
    pub(super) const fn describe(self) -> &'static str {
        match self {
            Self::Changed => "differs from the pinned temper",
            Self::Missing => "is missing from the vendored copy",
            Self::Extra => "is not in the pinned temper",
            Self::NotRegular => "is not a regular file",
        }
    }
}

/// Every drifted path of the PR's copy (`head`, the files under [`DIR`]), as a repository path
/// in path order. No files at all means no vendored copy, which is not drift.
pub(super) fn drift(pinned: &Pinned, head: Vec<TreeFile>) -> BTreeMap<String, Drift> {
    let mut drifted = BTreeMap::new();
    if head.is_empty() {
        return drifted;
    }
    let mut unseen: BTreeSet<&str> = pinned.keys().map(String::as_str).collect();
    for file in head {
        let relative = file
            .path
            .strip_prefix(DIR)
            .and_then(|rest| rest.strip_prefix('/'));
        let state = match (relative, &file.content) {
            (Some(relative), FileContent::Bytes(bytes)) => {
                unseen.remove(relative);
                match pinned.get(relative) {
                    Some(pin) if pin == bytes => None,
                    Some(_) => Some(Drift::Changed),
                    None => Some(Drift::Extra),
                }
            }
            (Some(relative), FileContent::NotRegular) => {
                unseen.remove(relative);
                Some(Drift::NotRegular)
            }
            // `.claude/temper` itself is a file or a link, not a directory.
            (None, _) => Some(Drift::NotRegular),
        };
        if let Some(state) = state {
            drifted.insert(file.path, state);
        }
    }
    drifted.extend(
        unseen
            .into_iter()
            .map(|relative| (format!("{DIR}/{relative}"), Drift::Missing)),
    );
    drifted
}
