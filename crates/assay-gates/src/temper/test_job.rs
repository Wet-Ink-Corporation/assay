//! The temper adapter in `gates-test`, zone Z2b, on its own VM (`docs/architecture/03-contracts.md`
//! §2.1 step 7, ADR-12, 05 T6): the plugin's declared `test` steps (`tests`, `doctests`) in file
//! order, each under its own timeout, so a hostile test can lie only about test results.
//!
//! Nothing here is read back from the tree: the steps' exit codes are the whole result. A step
//! that exits non-zero fails the gate (with `--no-tests=fail`, nextest exits non-zero when no
//! test runs, so zero tests fail); the next step still runs. A step that does not exit (timeout,
//! signal after its one retry, output cap) ends the run as that termination, a `gate-error`.

use assay_app::{BuildJob, GateJob, GateRun, Plugin, Step, Termination};
use assay_domain::GateConclusion;

use super::steps::stopped;
use super::{ErrorOf, TemperAdapter, TemperError};
use crate::capture::CaptureSupervisor;
use crate::revision::RevisionFiles;

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// Runs `gates-test` for `plugin`, whose declared steps are `steps`.
    pub(super) fn run_test(
        &mut self,
        plugin: &Plugin,
        pin: String,
        steps: &[Step],
    ) -> Result<GateRun, ErrorOf<S, R>> {
        let tests: Vec<&Step> = steps
            .iter()
            .filter(|step| step.job() == BuildJob::Test)
            .collect();
        if tests.is_empty() {
            // Nothing to run is never a pass; `run_gates` does not schedule such a plugin here.
            return Err(TemperError::UnsupportedJob {
                job: GateJob::Build(BuildJob::Test),
            });
        }
        let mut failed = false;
        for step in tests {
            match self.step_retried(step)? {
                Termination::Exited(0) => {}
                Termination::Exited(_) => failed = true,
                other => return Ok(stopped(other)),
            }
        }
        let conclusion = if failed {
            GateConclusion::Fail
        } else {
            GateConclusion::Pass
        };
        self.document(plugin, pin, (conclusion, Vec::new()))
    }
}
