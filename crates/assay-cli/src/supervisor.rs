//! `assay-forge`'s process-group supervisor, lent to the gate and model-runner adapters, which
//! may not depend on `assay-forge` (ADR-07, wi-603c63).

use std::io;
use std::process::Command;

use assay_app::Termination;
use assay_forge::process::{self, GroupKiller, Limits, ProcessError};
use assay_gates::{ProcessSpec, Supervisor};
use assay_runner::GroupKill;

/// The process supervision behind every gate and model process: each runs in its own process
/// group, which is killed whole and shown gone (H-13).
///
/// As an [`assay_gates::Supervisor`] it runs a gate under [`process::run`]; as an
/// [`assay_runner::GroupKill`] it kills `claude`'s group with [`GroupKiller::kill_group`].
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
        match process::run(command, &[], limits, &self.killer) {
            Ok(captured) => Ok(captured
                .status
                .code()
                .map_or(Termination::Signalled, Termination::Exited)),
            Err(ProcessError::TimedOut) => Ok(Termination::TimedOut),
            Err(ProcessError::OutputTooLarge) => Ok(Termination::OutputTooLarge),
            Err(error) => Err(error),
        }
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
