//! The PR's vendored temper (`.claude/temper/`) against the pinned copy (05 T4): assay never
//! runs the PR's copy, and reports its drift from the pin.
//!
//! The pinned copy is a directory in temper's vendored layout (`temper.mjs`, `lib/**`,
//! `VERSION`), prepared from the base's pin by the job. The PR's copy drifts when a file
//! differs in bytes, is missing, is extra, or is not a regular file. Bytes are compared by git
//! blob id, so no blob of the PR's copy is read. A PR with no vendored copy has nothing to
//! drift.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::revision::{EntryKind, TreeEntry};

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

/// The pinned copy as git blob ids: each file's path relative to its root and its id.
pub(super) type PinnedIds = BTreeMap<String, String>;

/// Every drifted path of the PR's copy, as a repository path in path order. `head` is any
/// listing of the head tree; only [`DIR`] and the entries under it are compared. No entries
/// there means no vendored copy, which is not drift.
pub(super) fn drift<'h>(
    pinned: &PinnedIds,
    head: impl IntoIterator<Item = &'h TreeEntry>,
) -> BTreeMap<Cow<'h, str>, Drift> {
    let mut drifted = BTreeMap::new();
    let mut vendored = false;
    let mut unseen: BTreeSet<&str> = pinned.keys().map(String::as_str).collect();
    for entry in head {
        let Some(rest) = entry.path.strip_prefix(DIR) else {
            continue;
        };
        let state = match (rest.strip_prefix('/'), entry.kind) {
            (Some(relative), EntryKind::File) => {
                unseen.remove(relative);
                match pinned.get(relative) {
                    Some(id) if *id == entry.object => None,
                    Some(_) => Some(Drift::Changed),
                    None => Some(Drift::Extra),
                }
            }
            (Some(relative), EntryKind::Symlink | EntryKind::Submodule) => {
                unseen.remove(relative);
                Some(Drift::NotRegular)
            }
            // `.claude/temper` itself is a file, a link or a submodule, not a directory.
            (None, _) if rest.is_empty() => Some(Drift::NotRegular),
            // A sibling such as `.claude/temper.json`.
            (None, _) => continue,
        };
        vendored = true;
        if let Some(state) = state {
            drifted.insert(Cow::Borrowed(entry.path.as_str()), state);
        }
    }
    if !vendored {
        return drifted;
    }
    drifted.extend(
        unseen
            .into_iter()
            .map(|relative| (Cow::Owned(format!("{DIR}/{relative}")), Drift::Missing)),
    );
    drifted
}
