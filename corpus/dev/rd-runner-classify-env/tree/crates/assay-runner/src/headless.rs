//! The `ClaudeHeadless` adapter: `claude -p` under the hardened headless profile (03 §8).

mod env;
mod supervise;
mod workdir;

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use assay_app::{ModelCall, ModelResult, ModelRunner};

pub use env::{EnvVarError, HeadlessEnv};
pub use workdir::{Workdir, WorkdirError};

use crate::stream;

/// The transcript cap when none is set: 32 MiB.
const DEFAULT_MAX_OUTPUT_BYTES: u64 = 32 * 1024 * 1024;

/// The only tools a headless call has (03 §8): no `Bash`, no web, no writes.
const TOOLS: &str = "Read,Grep,Glob";

/// `--settings` (03 §8): hooks off, reads of `/proc`, `/sys`, `~` and `/etc` denied, and
/// `blockReadsOutsideWorkingDirectories` nested under `permissions`, where the CLI reads it.
const SETTINGS: &str = r#"{"disableAllHooks":true,"permissions":{"deny":["Read(//proc/**)","Read(//sys/**)","Read(~/**)","Read(//etc/**)"],"blockReadsOutsideWorkingDirectories":true}}"#;

/// A [`ModelRunner`] that runs `claude -p` with the headless profile of 03 §8, authenticated by
/// an OAuth token.
///
/// Every call runs in the same empty [`Workdir`] with the [`HeadlessEnv`] environment, under the
/// call's timeout and this runner's transcript cap. Its transcript is classified like a
/// `Replay` recording: a timeout is charged at the call's budget, as is a transcript cut off at
/// the cap (reported [`Malformed`](assay_app::ModelOutcome::Malformed)). The exit status is not
/// consulted: the transcript's `result` message decides. The CLI's stderr is discarded.
#[derive(Debug)]
pub struct ClaudeHeadless {
    program: PathBuf,
    workdir: Workdir,
    env: HeadlessEnv,
    max_output_bytes: u64,
}

/// Why a headless call could not be made or supervised.
///
/// Up to and including [`Spawn`](Self::Spawn), the CLI never ran and nothing is charged. After
/// it, the CLI may have run and its cost is unknown: the caller charges the call's budget.
#[derive(Debug, thiserror::Error)]
pub enum HeadlessError {
    /// A path in the call is relative, so it would resolve against the empty working directory.
    #[error("{path} is not an absolute path")]
    RelativePath {
        /// The relative path.
        path: PathBuf,
    },
    /// The working directory is unusable, or no longer empty.
    #[error(transparent)]
    Workdir(#[from] WorkdirError),
    /// The timeout reaches past what the clock can represent.
    #[error("the timeout of {timeout:?} cannot be represented")]
    TimeoutTooLong {
        /// The requested timeout.
        timeout: Duration,
    },
    /// The temporary file holding the transcript could not be created or read.
    #[error("the transcript spool failed")]
    Spool(#[source] io::Error),
    /// The CLI could not be started.
    #[error("the claude CLI could not be started")]
    Spawn(#[source] io::Error),
    /// The running CLI could not be polled or reaped.
    #[error("the claude CLI could not be supervised")]
    Supervise(#[source] io::Error),
    /// The CLI could not be killed and may still be running: abort the job rather than retry.
    #[error("the claude CLI could not be killed")]
    Kill(#[source] io::Error),
}

impl ClaudeHeadless {
    /// A runner that starts `program` (the pinned `claude` binary) in `workdir` with `env`, and
    /// caps each transcript at 32 MiB.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, workdir: Workdir, env: HeadlessEnv) -> Self {
        Self {
            program: program.into(),
            workdir,
            env,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// This runner with each transcript capped at `bytes`.
    #[must_use]
    pub fn with_max_output_bytes(self, bytes: u64) -> Self {
        Self {
            max_output_bytes: bytes,
            ..self
        }
    }

    fn command(&self, call: &ModelCall) -> Command {
        let mut command = Command::new(&self.program);
        command
            .arg("-p")
            .arg(call.user_prompt.as_str())
            .args(["--output-format", "stream-json", "--verbose"])
            .arg("--json-schema")
            .arg(&call.json_schema)
            .arg("--append-system-prompt-file")
            .arg(&call.system_prompt_file)
            .arg("--exclude-dynamic-system-prompt-sections")
            .args(["--restricted", "--safe-mode"])
            .args(["--settings", SETTINGS])
            .args(["--tools", TOOLS])
            .arg("--add-dir")
            .arg(&call.add_dir)
            .args(["--permission-mode", "dontAsk"])
            .arg("--max-turns")
            .arg(call.max_turns.to_string())
            .arg("--max-budget-usd")
            .arg(call.budget.to_string())
            .current_dir(self.workdir.path());
        self.env.apply(&mut command);
        command
    }
}

impl ModelRunner for ClaudeHeadless {
    type Error = HeadlessError;

    /// Runs `call` once under the headless profile.
    ///
    /// # Errors
    /// Returns [`HeadlessError`] when a path in `call` is relative, the working directory is not
    /// empty, or the CLI could not be started, supervised or killed.
    fn run(&self, call: &ModelCall) -> Result<ModelResult, HeadlessError> {
        for path in [&call.system_prompt_file, &call.add_dir] {
            if !path.is_absolute() {
                return Err(HeadlessError::RelativePath { path: path.clone() });
            }
        }
        self.workdir.ensure_empty()?;
        let captured = supervise::run(self.command(call), call.timeout, self.max_output_bytes)?;
        Ok(stream::classify(
            &captured.transcript,
            captured.ending,
            call.budget,
        ))
    }
}
