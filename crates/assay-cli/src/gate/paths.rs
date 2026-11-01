//! The paths a gates job is given, refused or resolved before anything runs (05 T3).

use std::fs;
use std::io;
use std::path::{self, Path, PathBuf};

use super::{GateCliError, Program, StaticError};

/// `path` made absolute against the working directory, without touching the file system.
pub(super) fn absolute(path: &Path) -> Result<PathBuf, GateCliError> {
    path::absolute(path).map_err(|source| GateCliError::Path {
        path: path.to_path_buf(),
        source,
    })
}

/// `path` with every symlink resolved.
fn canonical(path: &Path) -> Result<PathBuf, GateCliError> {
    fs::canonicalize(path).map_err(|source| GateCliError::Path {
        path: path.to_path_buf(),
        source,
    })
}

/// The job's paths once checked, every symlink resolved: the ones it uses.
pub(super) struct Placed {
    pub(super) config: PathBuf,
    pub(super) head: PathBuf,
    pub(super) out: PathBuf,
}

/// Refuses, before anything runs, a head tree that is not a directory, a config that is not a
/// regular file outside it, and an `out` that exists in any form (a symlink included), whose
/// parent is not a directory, or that is inside the head tree. Containment is compared on
/// paths with every symlink resolved; `out`'s parent (it does not exist yet) must already be
/// written resolved, with no symlink and no `..`. Returns the resolved paths, the ones used: a
/// head tree's path with `..` in it would put the git ceiling (its parent) below it.
pub(super) fn refuse_paths(config: &Path, head: &Path, out: &Path) -> Result<Placed, GateCliError> {
    let head = canonical(head)?;
    if !head.is_dir() {
        return Err(GateCliError::HeadNotDir(head));
    }
    let config = canonical(config)?;
    if config.starts_with(&head) {
        return Err(GateCliError::ConfigInHead(config));
    }
    if !config.is_file() {
        return Err(GateCliError::NotAFile(config));
    }
    match fs::symlink_metadata(out) {
        Ok(_) => return Err(GateCliError::OutExists(out.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(GateCliError::Path {
                path: out.to_path_buf(),
                source,
            });
        }
    }
    // An `out` ending in `..` names no new directory.
    let (Some(parent), Some(name)) = (out.parent(), out.file_name()) else {
        return Err(GateCliError::Usage);
    };
    let resolved = canonical(parent)?;
    // A symlink or `..` on the way is refused rather than followed: a test could retarget a
    // symlink between this check and the writes.
    if resolved != parent {
        return Err(GateCliError::OutNotResolved {
            given: out.to_path_buf(),
            resolved,
        });
    }
    let out = resolved.join(name);
    if out.starts_with(&head) {
        return Err(GateCliError::OutInHead(out));
    }
    Ok(Placed { config, head, out })
}

/// `gates-static`'s programs: `node` and the pinned temper copy, each named absolute.
#[derive(Debug)]
pub(super) struct Programs {
    pub(super) node: PathBuf,
    pub(super) temper: PathBuf,
}

impl Programs {
    /// `node` and `temper` as given, refused unless each is absolute: a relative one would be
    /// resolved against the working directory, which the job does not choose.
    pub(super) fn absolute(node: PathBuf, temper: PathBuf) -> Result<Self, StaticError> {
        let absolute = |program: Program, path: PathBuf| {
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(StaticError::RelativeProgram { program, path })
            }
        };
        Ok(Self {
            node: absolute(Program::Node, node)?,
            temper: absolute(Program::Temper, temper)?,
        })
    }

    /// The programs with every symlink resolved, refused when either resolves inside `head`
    /// (resolved): the PR would choose what runs as the pinned temper.
    pub(super) fn outside(self, head: &Path) -> Result<Self, GateCliError> {
        let resolve = |program: Program, path: &Path| -> Result<PathBuf, GateCliError> {
            let path = canonical(path)?;
            if path.starts_with(head) {
                return Err(StaticError::ProgramInHead { program, path }.into());
            }
            Ok(path)
        };
        Ok(Self {
            node: resolve(Program::Node, &self.node)?,
            temper: resolve(Program::Temper, &self.temper)?,
        })
    }
}
