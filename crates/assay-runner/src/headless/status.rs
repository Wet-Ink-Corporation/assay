//! `ClaudeHeadless` as the preflight's [`CliProbe`] (06 §1, #83): `claude --version` and
//! `claude auth status`, each run like a model call (the same program, empty working directory,
//! cleared environment and process-group kill) under a short timeout and a small output cap.

use std::fmt;
use std::time::Duration;

use assay_app::{CliProbe, CliStatus, StatusReadError};

use super::supervise::{self, Bounds};
use super::{ClaudeHeadless, GroupKill, HeadlessError};
use crate::status::read_cli_status;
use crate::stream::Ending;

/// The most bytes of either status output read: 64 KiB. The 2.1.296 outputs are under 400.
const MAX_STATUS_BYTES: u64 = 64 * 1024;

/// The default wall-clock limit of each status command.
pub(super) const DEFAULT_STATUS_TIMEOUT: Duration = Duration::from_secs(30);

/// One of the two commands the preflight runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCommand {
    /// `claude --version`.
    Version,
    /// `claude auth status`.
    AuthStatus,
}

impl StatusCommand {
    const fn args(self) -> &'static [&'static str] {
        match self {
            Self::Version => &["--version"],
            Self::AuthStatus => &["auth", "status"],
        }
    }
}

impl fmt::Display for StatusCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Version => "claude --version",
            Self::AuthStatus => "claude auth status",
        })
    }
}

/// Why the CLI's status could not be read. Every variant fails the preflight.
#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    /// The command could not be started, supervised or killed, or the working directory is
    /// unusable.
    #[error(transparent)]
    Run(#[from] HeadlessError),
    /// The command did not finish within its timeout and was killed.
    #[error("`{command}` did not finish in time")]
    TimedOut {
        /// The command.
        command: StatusCommand,
    },
    /// The command wrote more than 64 KiB and was killed.
    #[error("`{command}` wrote more than {MAX_STATUS_BYTES} bytes")]
    TooLarge {
        /// The command.
        command: StatusCommand,
    },
    /// The outputs could not be read.
    #[error(transparent)]
    Read(#[from] StatusReadError),
}

impl<K> ClaudeHeadless<K> {
    /// This runner with each status command limited to `timeout` (30 s by default).
    #[must_use]
    pub fn with_status_timeout(self, timeout: Duration) -> Self {
        Self {
            status_timeout: timeout,
            ..self
        }
    }
}

impl<K: GroupKill> ClaudeHeadless<K> {
    /// The output of `command`, run like a model call with an empty stdin.
    fn status_output(&self, command: StatusCommand) -> Result<Vec<u8>, StatusError> {
        self.workdir.ensure_empty().map_err(HeadlessError::from)?;
        let mut process = self
            .parent_death
            .command(&self.program)
            .map_err(HeadlessError::Spawn)?;
        process
            .args(command.args())
            .current_dir(self.workdir.path());
        self.env.apply(&mut process);
        let bounds = Bounds {
            timeout: self.status_timeout,
            cap: MAX_STATUS_BYTES,
        };
        let captured = supervise::run(process, &[], bounds, &self.killer)?;
        match captured.ending {
            Ending::Exited => Ok(captured.transcript),
            Ending::TimedOut => Err(StatusError::TimedOut { command }),
            Ending::OutputCapped => Err(StatusError::TooLarge { command }),
        }
    }
}

impl<K: GroupKill> CliProbe for ClaudeHeadless<K> {
    type Error = StatusError;

    /// Runs `claude --version`, then `claude auth status`, and reads both. The exit status is
    /// not consulted: the outputs decide.
    ///
    /// # Errors
    /// [`StatusError`] when the working directory is not empty, a command could not be run,
    /// timed out or wrote past 64 KiB, or the outputs could not be read.
    fn status(&self) -> Result<CliStatus, StatusError> {
        let version = self.status_output(StatusCommand::Version)?;
        let auth_status = self.status_output(StatusCommand::AuthStatus)?;
        Ok(read_cli_status(&version, &auth_status)?)
    }
}
