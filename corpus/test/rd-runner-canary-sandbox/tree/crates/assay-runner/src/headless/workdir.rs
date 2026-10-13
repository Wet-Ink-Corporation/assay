//! The empty working directory every headless call runs in (03 §8).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// An absolute, empty directory: the working directory of every `claude -p` call, the same path
/// for every call (03 §8). It is checked again before each call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workdir {
    path: PathBuf,
}

/// Why a directory cannot serve as the working directory.
#[derive(Debug, thiserror::Error)]
pub enum WorkdirError {
    /// The path is relative.
    #[error("the working directory {path} is not an absolute path")]
    NotAbsolute {
        /// The rejected path.
        path: PathBuf,
    },
    /// The directory could not be listed (absent, not a directory, or not readable).
    #[error("the working directory {path} cannot be listed")]
    Unreadable {
        /// The directory.
        path: PathBuf,
        /// The listing failure.
        #[source]
        source: io::Error,
    },
    /// The directory holds an entry.
    #[error("the working directory {path} is not empty")]
    NotEmpty {
        /// The directory.
        path: PathBuf,
    },
}

impl Workdir {
    /// Takes `path` as the working directory.
    ///
    /// # Errors
    /// Returns [`WorkdirError::NotAbsolute`], [`WorkdirError::Unreadable`] or
    /// [`WorkdirError::NotEmpty`].
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, WorkdirError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(WorkdirError::NotAbsolute { path });
        }
        let workdir = Self { path };
        workdir.ensure_empty()?;
        Ok(workdir)
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Fails unless the directory can be listed and holds nothing.
    pub(super) fn ensure_empty(&self) -> Result<(), WorkdirError> {
        let unreadable = |source| WorkdirError::Unreadable {
            path: self.path.clone(),
            source,
        };
        match fs::read_dir(&self.path).map_err(unreadable)?.next() {
            None => Ok(()),
            Some(Ok(_)) => Err(WorkdirError::NotEmpty {
                path: self.path.clone(),
            }),
            Some(Err(source)) => Err(unreadable(source)),
        }
    }
}
