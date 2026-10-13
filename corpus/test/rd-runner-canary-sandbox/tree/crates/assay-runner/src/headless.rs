//! The `ClaudeHeadless` adapter: `claude -p` under the hardened headless profile (03 §8).

mod canary;
mod env;
mod group;
mod supervise;
mod workdir;

use std::io;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use assay_app::{ModelCall, ModelResult, ModelRunner};
use assay_domain::Aeu;

pub use canary::{CanaryError, ClaudeCanary};
pub use env::{EnvVarError, HeadlessEnv};
pub use group::GroupKill;
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
///
/// The CLI leads its own process group, and `killer` kills that whole group: at the timeout or
/// the cap before the CLI is reaped, and after it exits, so no process it started outlives the
/// call. A call returns only once the group has been shown gone, or with
/// [`HeadlessError::Kill`].
#[derive(Debug)]
pub struct ClaudeHeadless<K> {
    program: PathBuf,
    workdir: Workdir,
    env: HeadlessEnv,
    killer: K,
    max_output_bytes: u64,
}

/// Why a headless call could not be made or supervised.
///
/// [`may_have_run`](Self::may_have_run) tells the caller whether to charge: the variants up to
/// and including [`Spawn`](Self::Spawn) fail before the CLI starts, so nothing is charged; after
/// it ([`Supervise`](Self::Supervise), [`Kill`](Self::Kill), [`ReadBack`](Self::ReadBack)) the
/// CLI may have run and its cost is unknown, so the caller charges the call's budget.
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
    /// The temporary files for the prompt (stdin) and the transcript could not be created,
    /// before the CLI started.
    #[error("the prompt or transcript spool could not be created")]
    SpoolCreate(#[source] io::Error),
    /// The CLI could not be started.
    #[error("the claude CLI could not be started")]
    Spawn(#[source] io::Error),
    /// The running CLI could not be polled or reaped.
    #[error("the claude CLI could not be supervised")]
    Supervise(#[source] io::Error),
    /// The CLI, or a process in its group, could not be killed and may still be running: abort
    /// the job rather than retry.
    #[error("the claude CLI or its process group could not be killed")]
    Kill(#[source] io::Error),
    /// The transcript could not be read back after the CLI stopped.
    #[error("the transcript could not be read back")]
    ReadBack(#[source] io::Error),
}

impl HeadlessError {
    /// Whether the CLI may have run before this failure, so that its unknown cost must be
    /// charged at the call's budget. `false` means the CLI was never started.
    #[must_use]
    pub const fn may_have_run(&self) -> bool {
        match self {
            Self::RelativePath { .. }
            | Self::Workdir(_)
            | Self::TimeoutTooLong { .. }
            | Self::SpoolCreate(_)
            | Self::Spawn(_) => false,
            Self::Supervise(_) | Self::Kill(_) | Self::ReadBack(_) => true,
        }
    }
}

impl<K> ClaudeHeadless<K> {
    /// A runner that starts `program` (the pinned `claude` binary) in `workdir` with `env`,
    /// kills the CLI's process group with `killer`, and caps each transcript at 32 MiB. Pass
    /// `&killer` to keep it.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, workdir: Workdir, env: HeadlessEnv, killer: K) -> Self {
        Self {
            program: program.into(),
            workdir,
            env,
            killer,
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
        let invocation = Invocation {
            json_schema: &call.json_schema,
            system_prompt_file: &call.system_prompt_file,
            add_dir: &call.add_dir,
            max_turns: call.max_turns,
            budget: call.budget,
        };
        invocation.command(&self.program, self.workdir.path(), &self.env)
    }
}

/// The per-call values of the 03 §8 argv, shared by [`ClaudeHeadless`] and [`ClaudeCanary`] so
/// the canary runs exactly the profile it vouches for.
struct Invocation<'a> {
    json_schema: &'a str,
    system_prompt_file: &'a Path,
    add_dir: &'a Path,
    max_turns: NonZeroU8,
    budget: Aeu,
}

impl Invocation<'_> {
    /// `program` with exactly the 03 §8 argv, run in `cwd` with `env` only.
    fn command(&self, program: &Path, cwd: &Path, env: &HeadlessEnv) -> Command {
        let mut command = Command::new(program);
        command
            .arg("-p")
            .args(["--output-format", "stream-json", "--verbose"])
            .arg("--json-schema")
            .arg(self.json_schema)
            .arg("--append-system-prompt-file")
            .arg(self.system_prompt_file)
            .arg("--exclude-dynamic-system-prompt-sections")
            .args(["--restricted", "--safe-mode"])
            .args(["--settings", SETTINGS])
            .args(["--tools", TOOLS])
            .arg("--add-dir")
            .arg(self.add_dir)
            .args(["--permission-mode", "dontAsk"])
            .arg("--max-turns")
            .arg(self.max_turns.to_string())
            .arg("--max-budget-usd")
            .arg(self.budget.to_string())
            .current_dir(cwd);
        env.apply(&mut command);
        command
    }
}

impl<K: GroupKill> ModelRunner for ClaudeHeadless<K> {
    type Error = HeadlessError;

    /// Runs `call` once under the headless profile.
    ///
    /// # Errors
    /// Returns [`HeadlessError`] when a path in `call` is relative, the working directory is not
    /// empty, or the CLI could not be started, supervised or killed (its group included).
    fn run(&self, call: &ModelCall) -> Result<ModelResult, HeadlessError> {
        for path in [&call.system_prompt_file, &call.add_dir] {
            if !path.is_absolute() {
                return Err(HeadlessError::RelativePath { path: path.clone() });
            }
        }
        self.workdir.ensure_empty()?;
        let captured = supervise::run(
            self.command(call),
            call.user_prompt.as_str().as_bytes(),
            supervise::Bounds {
                timeout: call.timeout,
                cap: self.max_output_bytes,
            },
            &self.killer,
        )?;
        Ok(stream::classify(
            &captured.transcript,
            captured.ending,
            call.budget,
        ))
    }
}
