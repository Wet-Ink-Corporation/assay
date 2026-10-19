//! `gh run download` (#273, wi-6b26d6): one workflow-run artifact, downloaded and extracted by
//! gh into a fresh scratch directory inside the transport's own, so no assay code ever unpacks
//! the zip. It runs as `gh api` does: through the [`GhRunner`], with exactly the transport's
//! environment and working directory, under its own deadline and output cap.

use std::fs;
use std::io;
use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use tempfile::TempDir;

use super::{GhApi, GhCall, GhRunner, excerpt};
use crate::github::Repository;
use crate::process::{Limits, ProcessError};

/// A download's deadline, from start until gh's process group is killed: long enough for the
/// largest bundle (96 MiB) on a slow link, and bounded (H-13).
pub const GH_DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(2);

/// The most bytes `gh run download` may write on stdout, and separately on stderr: it prints
/// nothing but an error.
const DOWNLOAD_OUTPUT_CAP: usize = 64 << 10;

/// Why `gh run download` left nothing to read.
#[derive(Debug, thiserror::Error)]
pub enum GhDownloadError {
    /// The scratch directory could not be created, or its path is not UTF-8 (gh takes it as an
    /// argument).
    #[error("could not create the download's scratch directory")]
    Scratch(#[source] io::Error),
    /// gh could not be run within its limits.
    #[error("gh run download did not complete")]
    Run(#[source] ProcessError),
    /// gh exited with a failure.
    // `{stderr:?}`: escaped, so gh's output cannot inject control sequences into a log.
    #[error("gh run download failed ({exit}): {stderr:?}")]
    Failed {
        /// How gh exited.
        exit: ExitStatus,
        /// The start of what gh wrote on stderr, read lossily, the token replaced by
        /// `<redacted>`.
        stderr: String,
    },
}

/// The directory one artifact was extracted into, removed by [`Downloaded::close`] (or, failing
/// that, with the transport's own directory).
#[derive(Debug)]
pub(crate) struct Downloaded(TempDir);

impl Downloaded {
    /// Where gh extracted the artifact.
    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }

    /// Removes the directory.
    pub(crate) fn close(self) -> io::Result<()> {
        self.0.close()
    }
}

impl<R: GhRunner> GhApi<R> {
    /// Downloads artifact `name` of run `run_id` of `repository` with `gh run download`, which
    /// extracts its one artifact straight into a new mode-0700 directory under the transport's
    /// scratch directory. `name` is passed as is: the caller has checked it is exactly the
    /// artifact it wants.
    ///
    /// # Errors
    ///
    /// [`GhDownloadError`] when the directory cannot be made, gh cannot run within its limits
    /// or gh exits with a failure. The directory is removed on every error.
    pub(crate) fn download(
        &self,
        repository: &Repository,
        run_id: NonZeroU64,
        name: &str,
    ) -> Result<Downloaded, GhDownloadError> {
        let dir = tempfile::Builder::new()
            .prefix("download-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(self.config.path())
            .map_err(GhDownloadError::Scratch)?;
        let dest = dir.path().to_str().ok_or_else(|| {
            GhDownloadError::Scratch(io::Error::new(
                io::ErrorKind::InvalidFilename,
                "the scratch directory's path is not UTF-8",
            ))
        })?;
        let repo = repository.to_string();
        let run = run_id.to_string();
        let args = [
            "run", "download", "--repo", &repo, "--name", name, "--dir", dest, "--", &run,
        ];
        let env = self.env();
        let call = GhCall {
            program: &self.program,
            args: &args,
            env: &env,
            cwd: self.config.path(),
            stdin: &[],
            limits: Limits {
                timeout: GH_DOWNLOAD_TIMEOUT,
                max_output_bytes: DOWNLOAD_OUTPUT_CAP,
            },
        };
        let captured = self.runner.run(&call).map_err(GhDownloadError::Run)?;
        if !captured.status.success() {
            return Err(GhDownloadError::Failed {
                exit: captured.status,
                stderr: excerpt(&captured.stderr, &self.token),
            });
        }
        Ok(Downloaded(dir))
    }
}
