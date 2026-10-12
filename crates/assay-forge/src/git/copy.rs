//! The sanitised manifest-only copy of a commit (`docs/architecture/03-contracts.md` §2.1,
//! `04-pipeline.md` §2.2): the tree the pinned base cargo reads in Z2s to compute the package
//! set, with no PR content but manifests and no PR configuration.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::GitError;
use super::repository::ObjectRepo;
use super::sha::CommitSha;
use super::tree::{BlobRef, EntryKind, TreeEntry, TreeLimits};

/// Files written with their content: what cargo reads to resolve the package set.
const MANIFESTS: [&str; 2] = ["Cargo.toml", "Cargo.lock"];

/// File names never written, in any directory: toolchain selection and git attributes.
const NEUTRALISED_FILES: [&str; 3] = ["rust-toolchain", "rust-toolchain.toml", ".gitattributes"];

/// A directory never written, at any depth: cargo's configuration.
const NEUTRALISED_DIR: &str = ".cargo";

/// A manifest-only copy written by [`ObjectRepo::manifest_copy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestCopy {
    root: PathBuf,
    manifests: Vec<String>,
}

impl ManifestCopy {
    /// The copy's directory, canonical, so it is the directory cargo reports paths under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The repo-relative paths of the manifests written with their content, in git's order;
    /// empty for a tree with no manifest.
    #[must_use]
    pub fn manifests(&self) -> &[String] {
        &self.manifests
    }
}

/// Why a manifest-only copy was not written. Each fails closed: the side has no package set.
#[derive(Debug, Error)]
pub enum CopyError {
    /// The tree could not be listed or its manifests read.
    #[error("the commit's tree could not be read")]
    Git(#[from] GitError),
    /// The directory for the copy already has contents, which cargo would read as the PR's.
    #[error("{} is not empty; the copy must start fresh", dir.display())]
    DirectoryNotEmpty {
        /// The directory that was refused.
        dir: PathBuf,
    },
    /// A tree path is not a canonical relative path, so writing it could leave the copy.
    #[error("the tree holds a path that is not canonical: {path:?}")]
    Path {
        /// The path, as git listed it.
        path: String,
    },
    /// The copy's directory, or a file in it, could not be inspected or written.
    #[error("could not write {}", path.display())]
    Write {
        /// The directory or file.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
}

impl ObjectRepo {
    /// Writes a sanitised manifest-only copy of `commit` into `into`, which must not exist or
    /// be empty: every `Cargo.toml` and `Cargo.lock` with its content, every other file and
    /// symlink as an empty regular file (so cargo still discovers `src/lib.rs`, `build.rs` and
    /// the other targets, and reads nothing else), no submodule, and nothing under a `.cargo`
    /// directory or named `rust-toolchain`, `rust-toolchain.toml` or `.gitattributes`, at any
    /// depth and in any ASCII case. A symlinked manifest becomes an empty one, which cargo
    /// refuses (fail closed). Listing and reading are bounded by `limits`.
    ///
    /// # Errors
    /// [`CopyError::DirectoryNotEmpty`] when `into` has contents, [`CopyError::Git`] when the
    /// tree cannot be listed or read within `limits`, [`CopyError::Path`] for a tree path with
    /// an empty, `.` or `..` segment, and [`CopyError::Write`] when the copy cannot be written.
    pub fn manifest_copy(
        &self,
        commit: &CommitSha,
        into: &Path,
        limits: TreeLimits,
    ) -> Result<ManifestCopy, CopyError> {
        prepare(into)?;
        let entries = self.list_tree(commit, None, limits)?;
        if let Some(entry) = entries.iter().find(|entry| !is_canonical(&entry.path)) {
            return Err(CopyError::Path {
                // The error owns the path it names.
                path: entry.path.clone(),
            });
        }
        let manifests: Vec<BlobRef<'_>> = entries
            .iter()
            .filter(|entry| role(entry) == Role::Manifest)
            .map(BlobRef::from)
            .collect();
        let contents = self.read_blobs(&manifests, limits)?;
        for entry in entries
            .iter()
            .filter(|entry| role(entry) == Role::Placeholder)
        {
            write_new(into, &entry.path, b"")?;
        }
        for (manifest, content) in manifests.iter().zip(&contents) {
            write_new(into, manifest.path, content)?;
        }
        let root = fs::canonicalize(into).map_err(|source| CopyError::Write {
            path: into.to_path_buf(),
            source,
        })?;
        let manifests = entries
            .into_iter()
            .filter(|entry| role(entry) == Role::Manifest)
            .map(|entry| entry.path)
            .collect();
        Ok(ManifestCopy { root, manifests })
    }
}

/// What the copy holds for a tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Nothing: a submodule, cargo configuration, a toolchain file or git attributes.
    Skipped,
    /// The blob's content: a `Cargo.toml` or `Cargo.lock` that is a file.
    Manifest,
    /// An empty regular file: any other file, and every symlink.
    Placeholder,
}

/// What the copy holds for `entry`.
fn role(entry: &TreeEntry) -> Role {
    match entry.kind {
        EntryKind::Submodule => Role::Skipped,
        _ if is_neutralised(&entry.path) => Role::Skipped,
        EntryKind::File | EntryKind::Executable if is_manifest(&entry.path) => Role::Manifest,
        EntryKind::File | EntryKind::Executable | EntryKind::Symlink => Role::Placeholder,
    }
}

/// Creates `into`, or checks that it is empty.
fn prepare(into: &Path) -> Result<(), CopyError> {
    let failed = |source| CopyError::Write {
        path: into.to_path_buf(),
        source,
    };
    match fs::read_dir(into) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(CopyError::DirectoryNotEmpty {
                    dir: into.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(into).map_err(failed)
        }
        Err(source) => Err(failed(source)),
    }
}

/// Writes `content` to a new file at `relative` under `root`, creating its directories. The
/// copy holds only what this wrote, none of it a symlink, so no path can lead out of it.
fn write_new(root: &Path, relative: &str, content: &[u8]) -> Result<(), CopyError> {
    let path = root.join(relative);
    let failed = |source| CopyError::Write {
        path: path.clone(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(failed)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(failed)?;
    file.write_all(content).map_err(failed)
}

/// Whether `path` is relative with no empty, `.` or `..` segment.
fn is_canonical(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

/// Whether `path` is cargo configuration, a toolchain file or a git attributes file.
fn is_neutralised(path: &str) -> bool {
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        let is_last = segments.peek().is_none();
        let neutralised = if is_last {
            NEUTRALISED_FILES
                .iter()
                .any(|name| segment.eq_ignore_ascii_case(name))
        } else {
            segment.eq_ignore_ascii_case(NEUTRALISED_DIR)
        };
        if neutralised {
            return true;
        }
    }
    false
}

/// Whether `path` names a manifest cargo reads.
fn is_manifest(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    MANIFESTS.contains(&name)
}
