//! The parent-death guard (#154): `claude` started under util-linux `setpriv --pdeathsig KILL`
//! when `setpriv` is found at a fixed path, so the CLI dies with the assay thread that started
//! it, even when assay is killed by a signal and no code of its own runs.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The owner, group and other execute bits of a file mode.
const EXECUTE_BITS: u32 = 0o111;

/// How the CLI is started: under `setpriv --pdeathsig KILL`, or directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParentDeath {
    setpriv: Option<PathBuf>,
}

impl ParentDeath {
    /// The paths `setpriv` is looked for at, in order. `PATH` is never searched.
    pub const SETPRIV_PATHS: [&str; 2] = ["/usr/bin/setpriv", "/bin/setpriv"];

    /// The guard of this host: `setpriv` at the first of [`SETPRIV_PATHS`](Self::SETPRIV_PATHS)
    /// that is usable, or none.
    #[must_use]
    pub fn probe() -> Self {
        Self::probe_paths(Self::SETPRIV_PATHS)
    }

    /// `setpriv` at the first of `candidates` that is usable, or none.
    ///
    /// A candidate is usable when it is an absolute path whose metadata, symlinks followed, is
    /// a regular file with an execute bit. Links are followed because merged-/usr systems (the
    /// GitHub Ubuntu images among them) make `/bin` a link to `usr/bin`; the fixed paths are
    /// system directories, so a link there is the system's. A candidate whose metadata cannot
    /// be read is not usable: the probe's answer for it is "no `setpriv` here", which
    /// `assay doctor` reports as the open parent-death gap.
    #[must_use]
    pub fn probe_paths<P: AsRef<Path>>(candidates: impl IntoIterator<Item = P>) -> Self {
        let setpriv = candidates
            .into_iter()
            .find(|candidate| is_usable(candidate.as_ref()))
            .map(|candidate| candidate.as_ref().to_path_buf());
        Self { setpriv }
    }

    /// The `setpriv` the CLI is started under, or `None` when it is started directly.
    #[must_use]
    pub fn setpriv(&self) -> Option<&Path> {
        self.setpriv.as_deref()
    }

    /// The command that starts `program`: `setpriv --pdeathsig KILL -- <program>` when a
    /// `setpriv` was found, else `program` itself. Arguments added to it go to `program`.
    ///
    /// `setpriv` arms the signal and execs `program`, which keeps its pid: the process group
    /// the child leads, and that the group kill names by the child's pid, is `program`'s.
    ///
    /// Under `setpriv` a `program` that cannot be executed would not fail the spawn: `setpriv`
    /// would start and exit 126 or 127 with no transcript, which a model call charges at its
    /// budget. So `program` is checked first, as given: symlinks followed, it must be a regular
    /// file with an execute bit. One removed between the check and the exec is that residual.
    ///
    /// # Errors
    /// Under `setpriv`, the error reading `program`'s metadata, or `PermissionDenied` when it
    /// is not a regular file with an execute bit.
    pub(super) fn command(&self, program: &Path) -> io::Result<Command> {
        let Some(setpriv) = &self.setpriv else {
            return Ok(Command::new(program));
        };
        if !is_executable_file(&fs::metadata(program)?) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not an executable file", program.display()),
            ));
        }
        let mut command = Command::new(setpriv);
        command.args(["--pdeathsig", "KILL", "--"]).arg(program);
        Ok(command)
    }
}

/// Whether `candidate` is absolute and, symlinks followed, a regular file with an execute bit.
fn is_usable(candidate: &Path) -> bool {
    candidate.is_absolute() && fs::metadata(candidate).is_ok_and(|meta| is_executable_file(&meta))
}

/// Whether `meta` is a regular file's with an execute bit.
fn is_executable_file(meta: &fs::Metadata) -> bool {
    meta.is_file() && meta.permissions().mode() & EXECUTE_BITS != 0
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// A file at `path` with `mode`.
    fn file(path: &Path, mode: u32) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(path, "#!/bin/sh\n")?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    #[test]
    fn a_regular_executable_file_is_the_setpriv() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let setpriv = dir.path().join("setpriv");
        file(&setpriv, 0o755)?;

        let guard = ParentDeath::probe_paths([&setpriv]);

        assert_eq!(guard.setpriv(), Some(setpriv.as_path()));
        Ok(())
    }

    #[test]
    fn a_symlink_to_an_executable_file_is_followed() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("setpriv.real");
        file(&target, 0o755)?;
        let link = dir.path().join("setpriv");
        symlink(&target, &link)?;

        let guard = ParentDeath::probe_paths([&link]);

        // The candidate is kept as named, as `/bin/setpriv` is through merged /usr's `/bin`.
        assert_eq!(guard.setpriv(), Some(link.as_path()));
        Ok(())
    }

    #[test]
    fn unusable_candidates_are_skipped_and_none_left_starts_directly()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("missing");
        let directory = dir.path().join("directory");
        fs::create_dir(&directory)?;
        let not_executable = dir.path().join("not-executable");
        file(&not_executable, 0o644)?;
        let dangling = dir.path().join("dangling");
        symlink(&missing, &dangling)?;
        let link_to_directory = dir.path().join("link-to-directory");
        symlink(&directory, &link_to_directory)?;

        let guard = ParentDeath::probe_paths([
            missing.as_path(),
            &directory,
            &not_executable,
            &dangling,
            &link_to_directory,
        ]);

        assert_eq!(guard, ParentDeath::default());
        assert_eq!(guard.setpriv(), None);
        Ok(())
    }

    #[test]
    fn a_relative_candidate_is_skipped_even_when_it_names_an_executable() {
        // Tests run in the package directory, where this stand-in is an executable file.
        let relative = Path::new("tests/fixtures/fake-claude");
        assert!(
            relative.is_file(),
            "the precondition: {relative:?} resolves"
        );

        let guard = ParentDeath::probe_paths([relative]);

        assert_eq!(guard.setpriv(), None);
    }

    #[test]
    fn the_first_usable_candidate_wins() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        file(&first, 0o700)?;
        file(&second, 0o755)?;
        let missing = dir.path().join("missing");

        let guard = ParentDeath::probe_paths([&missing, &first, &second]);

        assert_eq!(guard.setpriv(), Some(first.as_path()));
        Ok(())
    }

    #[test]
    fn the_fixed_paths_are_absolute_and_usr_bin_comes_first() {
        assert_eq!(
            ParentDeath::SETPRIV_PATHS,
            ["/usr/bin/setpriv", "/bin/setpriv"]
        );
    }
}
