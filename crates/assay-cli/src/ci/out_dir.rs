//! A job's `--out`: a directory that does not exist yet, outside every checkout.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{self, Path, PathBuf};

/// A directory a job will create and write its artifact's files into.
///
/// It is checked to not exist in any form (a dangling symlink included), to sit outside every
/// checkout (no ancestor holds a `.git` directory or file), and to have a parent named by its
/// resolved path (no symlink and no `..` on the way, which a process could retarget before the
/// writes). A relative path is taken from the working directory.
///
/// The check is made once, when the value is built; whoever creates the directory still does
/// so with [`fs::create_dir`], which refuses one that appeared meanwhile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutDir(PathBuf);

impl OutDir {
    /// `path` as a job's output directory.
    ///
    /// # Errors
    ///
    /// [`OutDirError::Exists`] when something is at `path`; [`OutDirError::NoName`] for a path
    /// ending in `..` or naming the root; [`OutDirError::NotResolved`] when its parent is not
    /// written as its resolved path; [`OutDirError::InCheckout`] when an ancestor holds `.git`;
    /// [`OutDirError::Path`] when a path cannot be made absolute, resolved or inspected.
    pub fn new(path: &Path) -> Result<Self, OutDirError> {
        let given = path::absolute(path).map_err(OutDirError::at(path))?;
        match fs::symlink_metadata(&given) {
            Ok(_) => return Err(OutDirError::Exists(given)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(OutDirError::Path {
                    path: given,
                    source,
                });
            }
        }
        let (Some(parent), Some(name)) = (given.parent(), given.file_name()) else {
            return Err(OutDirError::NoName(given));
        };
        let resolved = fs::canonicalize(parent).map_err(OutDirError::at(parent))?;
        if resolved != parent {
            return Err(OutDirError::NotResolved { given, resolved });
        }
        let out = resolved.join(name);
        for ancestor in resolved.ancestors() {
            let marker = ancestor.join(".git");
            match fs::symlink_metadata(&marker) {
                Ok(_) => {
                    return Err(OutDirError::InCheckout {
                        out,
                        checkout: ancestor.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(OutDirError::Path {
                        path: marker,
                        source,
                    });
                }
            }
        }
        Ok(Self(out))
    }

    /// The directory, absolute and resolved but for its own name.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Why a path was refused as a job's output directory.
#[derive(Debug)]
pub enum OutDirError {
    /// A path could not be made absolute, resolved or inspected.
    Path {
        /// The path.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
    /// Something already exists at the path.
    Exists(PathBuf),
    /// The path ends in `..` or names the root, so it names no new directory.
    NoName(PathBuf),
    /// The parent is reached through a symlink or `..`.
    NotResolved {
        /// The path as given, made absolute.
        given: PathBuf,
        /// Its parent, resolved.
        resolved: PathBuf,
    },
    /// The directory would be inside a checkout.
    InCheckout {
        /// The directory.
        out: PathBuf,
        /// The checkout's root: the ancestor holding `.git`.
        checkout: PathBuf,
    },
}

impl OutDirError {
    /// The failure to resolve or inspect `path`, for `map_err`.
    fn at(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Path {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for OutDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path { path, .. } => write!(f, "cannot resolve {}", path.display()),
            Self::Exists(path) => {
                write!(f, "the output directory {} already exists", path.display())
            }
            Self::NoName(path) => write!(
                f,
                "the output directory {} names no new directory",
                path.display()
            ),
            Self::NotResolved { given, resolved } => write!(
                f,
                "the output directory {} passes through a symlink or `..` (its parent is {}); \
                 name it by its resolved path",
                given.display(),
                resolved.display()
            ),
            Self::InCheckout { out, checkout } => write!(
                f,
                "the output directory {} is inside the checkout {}",
                out.display(),
                checkout.display()
            ),
        }
    }
}

impl Error for OutDirError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Path { source, .. } => Some(source),
            Self::Exists(_)
            | Self::NoName(_)
            | Self::NotResolved { .. }
            | Self::InCheckout { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use super::*;

    /// A scratch directory named by its resolved path, as `OutDir` compares paths.
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let root = fs::canonicalize(dir.path()).expect("the scratch directory resolves");
        (dir, root)
    }

    #[test]
    fn a_new_directory_under_a_resolved_parent_is_accepted() {
        let (_dir, root) = scratch();

        let out = OutDir::new(&root.join("plan-out")).expect("a new directory");

        assert_eq!(out.path(), root.join("plan-out"));
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_exists_in_any_form_is_refused() {
        let (_dir, root) = scratch();
        fs::create_dir(root.join("dir")).expect("a directory");
        fs::write(root.join("file"), "").expect("a file");
        symlink(root.join("nowhere"), root.join("dangling")).expect("a symlink");

        for name in ["dir", "file", "dangling"] {
            let path = root.join(name);

            assert_matches!(
                OutDir::new(&path),
                Err(OutDirError::Exists(refused)) if refused == path,
                "{name}"
            );
        }
    }

    #[test]
    fn a_directory_inside_a_checkout_is_refused() {
        for marker in ["dir", "file"] {
            let (_dir, root) = scratch();
            let checkout = root.join("checkout");
            fs::create_dir_all(checkout.join("sub")).expect("a checkout");
            // A clone has a `.git` directory; a linked worktree has a `.git` file.
            match marker {
                "dir" => fs::create_dir(checkout.join(".git")).expect("a .git directory"),
                _ => fs::write(checkout.join(".git"), "gitdir: x\n").expect("a .git file"),
            }

            assert_matches!(
                OutDir::new(&checkout.join("sub/out")),
                Err(OutDirError::InCheckout { out, checkout: found })
                    if out == checkout.join("sub/out") && found == checkout,
                "{marker}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_named_through_a_symlink_or_dot_dot_is_refused() {
        let (_dir, root) = scratch();
        fs::create_dir(root.join("real")).expect("a directory");
        symlink(root.join("real"), root.join("link")).expect("a symlink");

        for given in [root.join("link/out"), root.join("real/../real/out")] {
            assert_matches!(
                OutDir::new(&given),
                Err(OutDirError::NotResolved { given: refused, resolved })
                    if refused == given && resolved == root.join("real"),
                "{}",
                given.display()
            );
        }
    }

    #[test]
    fn a_path_naming_no_new_directory_is_refused() {
        let (_dir, root) = scratch();
        // Not there, so not refused as existing; but `..` names no directory to create.
        let given = root.join("missing/..");

        assert_matches!(
            OutDir::new(&given),
            Err(OutDirError::NoName(refused)) if refused == given
        );
    }

    #[test]
    fn a_missing_parent_is_refused_with_its_path() {
        let (_dir, root) = scratch();

        assert_matches!(
            OutDir::new(&root.join("missing/out")),
            Err(OutDirError::Path { path, .. }) if path == root.join("missing")
        );
    }

    #[test]
    fn a_relative_path_is_taken_from_the_working_directory() {
        let out = OutDir::new(Path::new("does-not-exist-plan-out"));

        let cwd = fs::canonicalize(".").expect("the working directory resolves");
        // The crate's own directory is inside this repository's checkout.
        assert_matches!(
            out,
            Err(OutDirError::InCheckout { out, .. }) if out == cwd.join("does-not-exist-plan-out")
        );
    }
}
