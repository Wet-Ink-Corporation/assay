//! The sanitised manifest-only copy of a commit (`docs/architecture/03-contracts.md` §2.1,
//! `04-pipeline.md` §2.2): the tree the pinned base cargo reads in Z2s to compute the package
//! set, with no PR content but manifests and no PR configuration.

mod link;

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
    symlinks: Vec<CopiedSymlink>,
}

/// A symlink of the tree that the copy accepted and wrote as an empty file, with where it
/// leads. Both are repo-relative, `/`-separated paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedSymlink {
    /// The symlink's own path.
    pub path: String,
    /// Its target, resolved lexically from the symlink's directory; it may name nothing.
    pub target: String,
}

impl ManifestCopy {
    /// Every symlink of the tree, in git's order, each accepted by the same rules: the
    /// copy's empty files, and the cargo configuration, toolchain files and attributes it
    /// never writes (#65 review round 4 F1). A build-time root that holds one but not its
    /// target must be widened by the caller (wi-b5affa).
    #[must_use]
    pub fn symlinks(&self) -> &[CopiedSymlink] {
        &self.symlinks
    }

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
    /// The commit's tree could not be listed within the limits.
    #[error("the commit's tree could not be listed")]
    List(#[source] GitError),
    /// The manifests and symlink targets could not be read within the limits.
    #[error("the commit's manifests and symlink targets could not be read")]
    Read(#[source] GitError),
    /// The directory for the copy could not be inspected.
    #[error("could not inspect {}", path.display())]
    Inspect {
        /// The directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
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
    /// A symlink that an empty file cannot stand in for: its target leaves the tree, is the
    /// root or another directory, or passes through another symlink.
    #[error("the tree holds a symlink the copy cannot stand in for: {path:?}")]
    Symlink {
        /// The symlink's path, as git listed it.
        path: String,
    },
    /// A submodule: its crates are in the real build but not in the copy.
    #[error("the tree holds a submodule: {path:?}")]
    Submodule {
        /// The submodule's path, as git listed it.
        path: String,
    },
    /// A directory of the copy could not be created.
    #[error("could not create {}", path.display())]
    CreateDir {
        /// The directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// A file of the copy could not be written.
    #[error("could not write {}", path.display())]
    WriteFile {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The written copy's directory could not be made canonical.
    #[error("could not canonicalize {}", path.display())]
    Canonicalize {
        /// The directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
}

impl ObjectRepo {
    /// Writes a sanitised manifest-only copy of `commit` into `into`, which must not exist or
    /// be empty: every `Cargo.toml` and `Cargo.lock` with its content, every other file and
    /// every symlink to an in-tree file as an empty regular file (so cargo still discovers
    /// `src/lib.rs`, `build.rs` and the other targets, and reads nothing else), and nothing
    /// under a `.cargo` directory or named `rust-toolchain`, `rust-toolchain.toml` or
    /// `.gitattributes`, at any depth and in any ASCII case. A symlinked manifest becomes an
    /// empty one, which cargo refuses (fail closed). Listing and reading are bounded by
    /// `limits`.
    ///
    /// A tree the copy cannot mirror for cargo's discovery is refused rather than copied with
    /// something missing: a submodule, and a symlink whose target leaves the tree, is the root
    /// or another directory, or passes through another symlink (any of them could hide a
    /// member that cargo's glob `members` finds in the real tree). Symlinks that are never
    /// written (under `.cargo`, toolchain files, attributes) are resolved and refused the
    /// same way, and reported in [`ManifestCopy::symlinks`] with the rest.
    ///
    /// # Errors
    /// [`CopyError::Inspect`] or [`CopyError::DirectoryNotEmpty`] for `into`,
    /// [`CopyError::List`] and [`CopyError::Read`] when the tree cannot be listed or read
    /// within `limits`, [`CopyError::Path`] for a tree path with an empty, `.` or `..`
    /// segment, [`CopyError::Submodule`] and [`CopyError::Symlink`] for a tree the copy cannot
    /// mirror, and [`CopyError::CreateDir`], [`CopyError::WriteFile`] or
    /// [`CopyError::Canonicalize`] when the copy cannot be written.
    pub fn manifest_copy(
        &self,
        commit: &CommitSha,
        into: &Path,
        limits: TreeLimits,
    ) -> Result<ManifestCopy, CopyError> {
        prepare(into)?;
        let entries = self
            .list_tree(commit, None, limits)
            .map_err(CopyError::List)?;
        let roles = entries
            .iter()
            .map(|entry| role(entry).map(|role| (entry, role)))
            .collect::<Result<Vec<_>, _>>()?;
        let read: Vec<(&TreeEntry, Role)> = roles
            .iter()
            .copied()
            .filter(|(_, role)| matches!(role, Role::Manifest | Role::Link | Role::HiddenLink))
            .collect();
        let blobs: Vec<BlobRef<'_>> = read
            .iter()
            .map(|(entry, _)| BlobRef::from(*entry))
            .collect();
        let contents = self.read_blobs(&blobs, limits).map_err(CopyError::Read)?;
        let tree = link::Tree::new(&entries);
        let mut symlinks = Vec::new();
        for ((entry, role), target) in read.iter().zip(&contents) {
            if *role == Role::Manifest {
                continue;
            }
            let Some(target) = tree.resolve(&entry.path, target) else {
                return Err(CopyError::Symlink {
                    // The error owns the path it names.
                    path: entry.path.clone(),
                });
            };
            symlinks.push(CopiedSymlink {
                // The copy reports the symlinks it accepted, so it owns their paths.
                path: entry.path.clone(),
                target,
            });
        }
        for (entry, _) in roles
            .iter()
            .filter(|(_, role)| matches!(role, Role::Placeholder | Role::Link))
        {
            write_new(into, &entry.path, b"")?;
        }
        let mut manifests = Vec::new();
        for ((entry, role), content) in read.iter().zip(&contents) {
            if *role == Role::Manifest {
                write_new(into, &entry.path, content)?;
                // The copy reports the paths it wrote, so it owns them.
                manifests.push(entry.path.clone());
            }
        }
        let root = fs::canonicalize(into).map_err(|source| CopyError::Canonicalize {
            path: into.to_path_buf(),
            source,
        })?;
        Ok(ManifestCopy {
            root,
            manifests,
            symlinks,
        })
    }
}

/// What the copy holds for a tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Nothing: cargo configuration, a toolchain file or git attributes that is not a
    /// symlink.
    Skipped,
    /// The blob's content: a `Cargo.toml` or `Cargo.lock` that is a file.
    Manifest,
    /// An empty regular file: any other file.
    Placeholder,
    /// An empty regular file, once its target (the blob) is shown to be no directory and
    /// inside the tree: a symlink.
    Link,
    /// Nothing, but resolved and reported like [`Role::Link`]: a symlink that is cargo
    /// configuration, a toolchain file or git attributes.
    HiddenLink,
}

/// What the copy holds for `entry`; a non-canonical path or a submodule is refused.
fn role(entry: &TreeEntry) -> Result<Role, CopyError> {
    if !is_canonical(&entry.path) {
        return Err(CopyError::Path {
            // The error owns the path it names.
            path: entry.path.clone(),
        });
    }
    Ok(match entry.kind {
        EntryKind::Submodule => {
            return Err(CopyError::Submodule {
                // The error owns the path it names.
                path: entry.path.clone(),
            });
        }
        // Every symlink is resolved and reported, neutralised or not (#65 review round 4 F1):
        // one under `.cargo` can still lie in an exec root.
        EntryKind::Symlink if is_neutralised(&entry.path) => Role::HiddenLink,
        EntryKind::Symlink => Role::Link,
        _ if is_neutralised(&entry.path) => Role::Skipped,
        EntryKind::File | EntryKind::Executable if is_manifest(&entry.path) => Role::Manifest,
        EntryKind::File | EntryKind::Executable => Role::Placeholder,
    })
}

/// Creates `into`, or checks that it is empty.
fn prepare(into: &Path) -> Result<(), CopyError> {
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
            fs::create_dir_all(into).map_err(|source| CopyError::CreateDir {
                path: into.to_path_buf(),
                source,
            })
        }
        Err(source) => Err(CopyError::Inspect {
            path: into.to_path_buf(),
            source,
        }),
    }
}

/// Writes `content` to a new file at `relative` under `root`, creating its directories. The
/// copy holds only what this wrote, none of it a symlink, so no path can lead out of it.
fn write_new(root: &Path, relative: &str, content: &[u8]) -> Result<(), CopyError> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CopyError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| file.write_all(content));
    written.map_err(|source| CopyError::WriteFile { path, source })
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
