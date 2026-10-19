//! `assay-forge`'s process-group supervisor, lent to the gate and model-runner adapters, which
//! may not depend on `assay-forge` (ADR-07, wi-603c63).

use std::io;
use std::process::Command;

use assay_app::Termination;
use assay_forge::github::{GhCall, GhRunner};
use assay_forge::process::{self, Bounds, Captured, GroupKiller, Limits, ProcessError};
use assay_gates::{CaptureSupervisor, CapturedRun, ProcessSpec, Supervisor};
use assay_runner::GroupKill;

/// The process supervision behind every gate and model process: each runs in its own process
/// group, which is killed whole and shown gone (H-13).
///
/// As an [`assay_gates::Supervisor`] it runs a gate under [`process::run`], and as an
/// [`assay_gates::CaptureSupervisor`] it also keeps the process's stdout (the pinned
/// `temper scan`); as an [`assay_runner::GroupKill`] it kills `claude`'s group with
/// [`GroupKiller::kill_group`]; as an [`assay_forge::github::GhRunner`] it runs the `gh api`
/// calls of the GitHub transport, with their stdin and both outputs kept (#273).
#[derive(Debug, Clone)]
pub struct ForgeSupervisor {
    killer: GroupKiller,
}

impl ForgeSupervisor {
    /// A supervisor that kills process groups with `killer`.
    #[must_use]
    pub const fn new(killer: GroupKiller) -> Self {
        Self { killer }
    }
}

impl Supervisor for ForgeSupervisor {
    type Error = ProcessError;

    /// Runs `spec` with exactly its environment, its stdin closed and its output discarded.
    ///
    /// A timeout and an over-cap output are terminations, reported once the group is shown
    /// gone.
    ///
    /// Cost: [`process::run`] reads stdout and stderr back into memory before this discards
    /// them, so a run briefly holds up to twice `spec.max_output_bytes` (32 MiB at
    /// `ExecLimits::DEFAULT`). Kept rather than adding a forge variant that skips the
    /// read-back, since one gate runs at a time (#150 review F4).
    ///
    /// # Errors
    ///
    /// [`ProcessError::Kill`] when the group could not be shown gone (abort the job), and the
    /// other [`ProcessError`]s when the gate could not be started or observed.
    fn run(&mut self, spec: &ProcessSpec<'_>) -> Result<Termination, ProcessError> {
        self.capture(spec).map(|run| run.termination)
    }
}

impl CaptureSupervisor for ForgeSupervisor {
    type Error = ProcessError;

    /// Runs `spec` like [`Supervisor::run`] and keeps its stdout; stderr is discarded. A run
    /// that timed out or passed the output cap keeps no stdout, so a truncated document is
    /// never handed on.
    ///
    /// # Errors
    ///
    /// As [`Supervisor::run`].
    fn capture(&mut self, spec: &ProcessSpec<'_>) -> Result<CapturedRun, ProcessError> {
        let mut command = Command::new(spec.program);
        command
            .args(spec.args)
            .current_dir(spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(name, value)| (name, value)));
        let limits = Limits {
            timeout: spec.timeout,
            max_output_bytes: spec.max_output_bytes,
        };
        let stopped = |termination| CapturedRun {
            termination,
            stdout: Vec::new(),
        };
        match process::run(command, &[], limits, &self.killer) {
            Ok(captured) => Ok(CapturedRun {
                termination: captured
                    .status
                    .code()
                    .map_or(Termination::Signalled, Termination::Exited),
                stdout: captured.stdout,
            }),
            Err(ProcessError::TimedOut) => Ok(stopped(Termination::TimedOut)),
            Err(ProcessError::OutputTooLarge) => Ok(stopped(Termination::OutputTooLarge)),
            Err(error) => Err(error),
        }
    }
}

impl GhRunner for ForgeSupervisor {
    /// Runs `call` under [`process::run_capped`] with exactly its environment, working
    /// directory, stdin, limits and directory cap.
    ///
    /// # Errors
    ///
    /// The [`ProcessError`] of [`process::run_capped`]: a timeout, an over-cap output or a
    /// directory past its cap once the group is shown gone, [`ProcessError::Kill`] when it
    /// could not be (abort the job).
    fn run(&self, call: &GhCall<'_>) -> Result<Captured, ProcessError> {
        let mut command = Command::new(call.program);
        command
            .args(call.args)
            .current_dir(call.cwd)
            .env_clear()
            .envs(call.env.iter().copied());
        let bounds = Bounds {
            limits: call.limits,
            directory: call.directory,
        };
        process::run_capped(command, call.stdin, bounds, &self.killer)
    }
}

impl GroupKill for ForgeSupervisor {
    fn kill_group(&self, group: u32) -> io::Result<()> {
        self.killer.kill_group(group)
    }

    fn kill_led_group(&self, group: u32) -> io::Result<()> {
        self.killer.kill_led_group(group)
    }
}
