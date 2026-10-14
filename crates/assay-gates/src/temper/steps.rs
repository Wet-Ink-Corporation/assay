//! The plugin's declared steps (`[[plugin.step]]`), run in their Z2b job from the head tree
//! (`docs/architecture/03-contracts.md` §1, §2.1 step 7): each under its own `timeout_s`, with
//! the gate environment. Shared by `gates-lint` (`fmt`, `deny`) and `gates-test` (`tests`,
//! `doctests`); both retry a step once when its run ended in an infra-class way (F9,
//! `06-operations.md` §4).

use std::path::Path;
use std::time::Duration;

use assay_app::{GateDocument, GateRun, Step, Termination};

use super::{ErrorOf, TemperAdapter, TemperError, TemperSettings};
use crate::capture::CaptureSupervisor;
use crate::exec::{ProcessSpec, resolve};
use crate::revision::RevisionFiles;

/// Most bytes of stdout (and, separately, stderr) of each step and of `baseline --check`.
const MAX_STEP_OUTPUT_BYTES: usize = 16 << 20;

/// A process run that ended without exiting: the gate use case reports its termination.
pub(super) const fn stopped(termination: Termination) -> GateRun {
    GateRun {
        termination,
        document: GateDocument::Missing,
    }
}

/// Whether a step's run ended in an infra-class way (F9): a signal it did not exit from, which
/// comes from outside the step (the runner, the kernel's OOM killer), since a test that crashes
/// is reported by the test runner's own exit code. Nothing else is: an exit code is the step's
/// verdict (cargo exits 101 for a registry failure and a compile error alike, and reading its
/// stderr would model its text), a timeout or an output cap would recur, and a supervisor error
/// may leave the process running (`ProcessError::Kill`: abort, never retry).
const fn infra_class(termination: Termination) -> bool {
    matches!(termination, Termination::Signalled)
}

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// Runs one declared step once, from the head tree with the gate environment under its own
    /// timeout.
    fn step(&mut self, step: &Step) -> Result<Termination, ErrorOf<S, R>> {
        let program = resolve(&self.settings.head_tree, step.command().program());
        capture(
            &mut self.supervisor,
            &self.settings,
            (
                step.name().as_str(),
                program.as_path(),
                step.command().args(),
            ),
            step.timeout(),
        )
    }

    /// Runs one declared step like [`step`](Self::step), and once more when the first run was
    /// infra-class; the last run's termination decides. At most two runs, so at most twice the
    /// step's `timeout_s`.
    pub(super) fn step_retried(&mut self, step: &Step) -> Result<Termination, ErrorOf<S, R>> {
        let first = self.step(step)?;
        if infra_class(first) {
            self.step(step)
        } else {
            Ok(first)
        }
    }
}

/// Runs `(process, program, args)` from the head tree with the gate environment under
/// `timeout`, once; `process` names it in errors.
pub(super) fn capture<S: CaptureSupervisor, R>(
    supervisor: &mut S,
    settings: &TemperSettings,
    (process, program, args): (&str, &Path, &[String]),
    timeout: Duration,
) -> Result<Termination, TemperError<S::Error, R>> {
    let spec = ProcessSpec {
        program,
        args,
        cwd: &settings.head_tree,
        env: &settings.scan_env,
        timeout,
        max_output_bytes: MAX_STEP_OUTPUT_BYTES,
    };
    supervisor
        .capture(&spec)
        .map(|run| run.termination)
        .map_err(|source| TemperError::Lint {
            process: process.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_signal_is_infra_class() {
        let terminations = [
            Termination::Exited(0),
            Termination::Exited(1),
            Termination::Exited(4),
            Termination::Exited(101),
            Termination::Signalled,
            Termination::TimedOut,
            Termination::OutputTooLarge,
        ];

        let infra: Vec<Termination> = terminations
            .into_iter()
            .filter(|termination| infra_class(*termination))
            .collect();

        assert_eq!(infra, [Termination::Signalled]);
    }
}
