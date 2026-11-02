//! The temper adapter in `gates-test`, zone Z2b, on its own VM (`docs/architecture/03-contracts.md`
//! §2.1 step 7, ADR-12, 05 T6): the plugin's declared `test` steps (`tests`, `doctests`) in file
//! order, each under its own timeout, so a hostile test can lie only about test results.
//!
//! Nothing here is read back from the tree: the steps' exit codes are the whole result. A step
//! that exits non-zero fails the gate (with `--no-tests=fail`, nextest exits non-zero when no
//! test runs, so zero tests fail); the next step still runs. A step that does not exit (timeout,
//! signal after its one retry, output cap) ends the run as that termination, a `gate-error`.
//!
//! [`TemperAdapter`] runs it in `gates-test`, and so does [`TemperTestAdapter`], which holds only
//! what this job needs: no `node`, no pinned temper, no reader of the PR's trees and no ratchet
//! base, so the job takes nothing that `gates-lint` uses (#67).

use std::convert::Infallible;

use assay_app::{Adapter, BuildJob, GateJob, GateRun, GateRunner, Plugin, Step, Termination};
use assay_domain::GateConclusion;

use super::steps::{TemperStepSettings, Verdict, step_retried, stopped};
use super::{ErrorOf, TemperAdapter, TemperError};
use crate::capture::CaptureSupervisor;
use crate::revision::RevisionFiles;

/// The job this file runs.
const TEST: GateJob = GateJob::Build(BuildJob::Test);

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// Runs `gates-test` for `plugin`, whose declared steps are `steps`.
    pub(super) fn run_test(
        &mut self,
        plugin: &Plugin,
        pin: String,
        steps: &[Step],
    ) -> Result<GateRun, ErrorOf<S, R>> {
        run_test(
            &mut self.supervisor,
            &self.settings.steps,
            (plugin, pin, steps),
        )
    }
}

/// The temper adapter for `gates-test` alone: the declared `test` steps of a temper plugin,
/// run through `supervisor` exactly as [`TemperAdapter`] runs them in that job.
#[derive(Debug)]
pub struct TemperTestAdapter<S> {
    supervisor: S,
    settings: TemperStepSettings,
}

impl<S> TemperTestAdapter<S> {
    /// An adapter that runs the steps through `supervisor` with `settings`. Pass
    /// `&mut supervisor` to keep it.
    pub const fn new(supervisor: S, settings: TemperStepSettings) -> Self {
        Self {
            supervisor,
            settings,
        }
    }
}

impl<S: CaptureSupervisor> GateRunner for TemperTestAdapter<S> {
    /// It reads no tree, so its reader error never occurs.
    type Error = TemperError<S::Error, Infallible>;

    /// In `gates-test`, runs the declared `test` steps and concludes `fail` when one exits
    /// non-zero; a step that does not exit is reported as that termination, and a step a
    /// signal ended is run once more, that run deciding.
    ///
    /// # Errors
    ///
    /// [`TemperError`] without running anything when `plugin` is not temper, `job` is not
    /// `gates-test` ([`TemperError::UnsupportedJob`]) or the plugin declares no `test` step;
    /// [`TemperError::Lint`] when a step cannot be run.
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        let Adapter::Temper { pin, steps } = plugin.adapter() else {
            return Err(TemperError::NotTemper {
                plugin: plugin.name().as_str().to_owned(),
            });
        };
        if job != TEST {
            return Err(TemperError::UnsupportedJob { job });
        }
        run_test(
            &mut self.supervisor,
            &self.settings,
            (plugin, pin.to_string(), steps),
        )
    }
}

/// Runs `gates-test` for `plugin` at `pin`, whose declared steps are `steps`.
fn run_test<S: CaptureSupervisor, R>(
    supervisor: &mut S,
    settings: &TemperStepSettings,
    (plugin, pin, steps): (&Plugin, String, &[Step]),
) -> Result<GateRun, TemperError<S::Error, R>> {
    let tests: Vec<&Step> = steps
        .iter()
        .filter(|step| step.job() == BuildJob::Test)
        .collect();
    if tests.is_empty() {
        // Nothing to run is never a pass; `run_gates` does not schedule such a plugin here.
        return Err(TemperError::UnsupportedJob { job: TEST });
    }
    let mut failed_steps = Vec::new();
    for step in tests {
        match step_retried(supervisor, settings, step)? {
            Termination::Exited(0) => {}
            Termination::Exited(_) => failed_steps.push(step.name().as_str().to_owned()),
            other => return Ok(stopped(other)),
        }
    }
    let conclusion = if failed_steps.is_empty() {
        GateConclusion::Pass
    } else {
        GateConclusion::Fail
    };
    // This job has no findings, so every failure is a failed step (#153).
    let verdict = Verdict {
        conclusion,
        findings: Vec::new(),
        failed_steps: Some(failed_steps),
    };
    settings.document(plugin, pin, verdict)
}
