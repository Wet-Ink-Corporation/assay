//! The run's artifacts as the workflow downloads them (#60 ST-14, D12 A): one
//! `download-artifact` step (`pattern: assay-*`) lays each artifact out as a directory of its
//! files under one root, and [`DirArtifacts`] reads them as `assay_app`'s [`RunArtifacts`].
//!
//! Only the names the authenticated plan gives are asked for, so anything else a job uploaded
//! is never read. Each name must be one plain path component, and each file a regular file
//! reached without a symlink, read under the caller's limit; the digest check is `decide`'s.

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use assay_app::{Fetched, RunArtifacts};

/// The run's downloaded artifacts: `<root>/<artifact>/<file>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirArtifacts {
    root: PathBuf,
}

impl DirArtifacts {
    /// The artifacts downloaded under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// Why an artifact's file could not be read. An absent artifact or file, or one over the
/// limit, is a [`Fetched`] instead.
#[derive(Debug)]
pub enum DirArtifactsError {
    /// The artifact or file name is not one plain path component (empty, `.`, `..`, or holding
    /// `/`, `\` or NUL): it is never joined onto the root.
    Name,
    /// The artifact is not a directory, or the file not a regular file, as named (a symlink
    /// is not followed).
    NotARegularFile {
        /// The path.
        path: PathBuf,
    },
    /// The file system could not be read.
    Read {
        /// The path.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
}

impl fmt::Display for DirArtifactsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name => f.write_str("an artifact or file name is not one plain path component"),
            Self::NotARegularFile { path } => {
                write!(f, "{} is not a regular file or directory", path.display())
            }
            Self::Read { path, .. } => write!(f, "cannot read {}", path.display()),
        }
    }
}

impl Error for DirArtifactsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Name | Self::NotARegularFile { .. } => None,
            Self::Read { source, .. } => Some(source),
        }
    }
}

impl RunArtifacts for DirArtifacts {
    type Error = DirArtifactsError;

    /// `<root>/<artifact>/<file>`, read under `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`DirArtifactsError::Name`] for a name that is not one plain component, before anything
    /// is read; [`DirArtifactsError::NotARegularFile`] when the artifact is not a directory or
    /// the file not a regular file (a symlink at either is refused, not followed);
    /// [`DirArtifactsError::Read`] when the file system cannot be read.
    fn fetch(&self, artifact: &str, file: &str, limit: usize) -> Result<Fetched, Self::Error> {
        if !is_component(artifact) {
            return Err(DirArtifactsError::Name);
        }
        read_in(&self.root.join(artifact), file, limit)
    }
}

/// File `file` of `directory`, read as [`DirArtifacts`] reads an artifact's: `directory` must
/// be a directory and `file` a regular file, neither reached through a symlink, read under
/// `limit` bytes. An absent directory or file is [`Fetched::Missing`].
pub(crate) fn read_in(
    directory: &Path,
    file: &str,
    limit: usize,
) -> Result<Fetched, DirArtifactsError> {
    if !is_component(file) {
        return Err(DirArtifactsError::Name);
    }
    match kind(directory)? {
        Kind::Absent => return Ok(Fetched::Missing),
        Kind::Directory => {}
        Kind::File | Kind::Other => {
            return Err(DirArtifactsError::NotARegularFile {
                path: directory.to_path_buf(),
            });
        }
    }
    let path = directory.join(file);
    match kind(&path)? {
        Kind::Absent => Ok(Fetched::Missing),
        Kind::File => read(&path, limit),
        Kind::Directory | Kind::Other => Err(DirArtifactsError::NotARegularFile { path }),
    }
}

/// What is at a path, the path itself not followed.
enum Kind {
    Absent,
    Directory,
    File,
    Other,
}

/// What is at `path`, without following a symlink there.
fn kind(path: &Path) -> Result<Kind, DirArtifactsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(Kind::Directory),
        Ok(metadata) if metadata.is_file() => Ok(Kind::File),
        Ok(_) => Ok(Kind::Other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Kind::Absent),
        Err(source) => Err(DirArtifactsError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The regular file at `path`, read under `limit` bytes: one byte more is read, so a file that
/// grew past the limit since it was listed is still too large.
fn read(path: &Path, limit: usize) -> Result<Fetched, DirArtifactsError> {
    let failed = |source| DirArtifactsError::Read {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(failed)?;
    // Opened by name after the check: what was opened must still be a regular file.
    if !file.metadata().map_err(failed)?.is_file() {
        return Err(DirArtifactsError::NotARegularFile {
            path: path.to_path_buf(),
        });
    }
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes).map_err(failed)?;
    if bytes.len() > limit {
        Ok(Fetched::TooLarge)
    } else {
        Ok(Fetched::Found(bytes))
    }
}

/// Whether `name` is one plain path component: not empty, not `.` or `..`, and without `/`,
/// `\` or NUL.
fn is_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}
