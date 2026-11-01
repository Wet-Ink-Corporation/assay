//! `gates-test` (Z2b, its own VM): temper's declared `test` steps and the `exec` gates whose
//! `job` is `test`. Nothing of `gates-lint` is taken: no `node`, no pinned temper, no object-only
//! repository, no ratchet base.

use std::convert::Infallible;
use std::fmt;
use std::path::Path;

use assay_app::{Adapter, GateJob, GateRun, GateRunner, Plugin, run_gates};
use assay_cli::ForgeSupervisor;
use assay_forge::process::{GroupKiller, ProcessError};
use assay_gates::{
    ExecAdapter, ExecError, ExecLimits, ExecSettings, GateEnvironment, TemperError,
    TemperStepSettings, TemperTestAdapter,
};
use tempfile::TempDir;

use super::{
    Finished, GateCliError, JobInputs, Paths, Prepared, Ran, TEST, finish, prepare, scratch,
};

/// Runs `gates-test` over `paths` and writes its documents and output.
///
/// # Errors
///
/// [`GateCliError`] as [`super::run`] says.
pub(super) fn run(paths: &Paths, job: JobInputs) -> Result<Ran, GateCliError> {
    let Prepared {
        placed,
        output,
        config,
        validator,
        killer,
    } = prepare(paths, &job.output)?;
    let scratch = scratch()?;
    let environment = GateEnvironment::from_vars(job.vars);
    let mut runner = TestGates::new(&placed.head, environment, job.run_key, (killer, &scratch))?;
    let reports = run_gates(&config, TEST, &mut runner, &validator);
    finish(
        Finished {
            job: TEST,
            config: &config,
            out: &placed.out,
            output,
        },
        reports,
        scratch,
    )
}

/// The gates `gates-test` runs: temper's declared `test` steps and the `exec` gates of the job.
struct TestGates {
    temper: TemperTestAdapter<ForgeSupervisor>,
    exec: ExecAdapter<ForgeSupervisor>,
}

impl TestGates {
    /// The adapters, run from `head` with `environment`; `exec` documents go under `scratch`.
    fn new(
        head: &Path,
        environment: GateEnvironment,
        run_key: String,
        (killer, scratch): (GroupKiller, &TempDir),
    ) -> Result<Self, GateCliError> {
        // Each settings value stores its own copy of the head tree's path.
        let steps = TemperStepSettings::new(head.to_path_buf(), &environment, run_key)
            .map_err(GateCliError::TemperSettings)?;
        let exec = ExecSettings::new(head, scratch.path(), environment, ExecLimits::DEFAULT)
            .map_err(GateCliError::ExecSettings)?;
        Ok(Self {
            // Each adapter owns a supervisor; the killer is a program path.
            temper: TemperTestAdapter::new(ForgeSupervisor::new(killer.clone()), steps),
            exec: ExecAdapter::new(ForgeSupervisor::new(killer), exec),
        })
    }
}

/// Why a gate of `gates-test` did not run.
#[derive(Debug)]
enum TestGateError {
    Temper(TemperError<ProcessError, Infallible>),
    Exec(ExecError<ProcessError>),
    /// A Weigh-In plugin, which has no work in `gates-test`.
    NotInJob,
}

impl fmt::Display for TestGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Temper(source) => write!(f, "{source}"),
            Self::Exec(source) => write!(f, "{source}"),
            Self::NotInJob => f.write_str("the plugin has no work in gates-test"),
        }
    }
}

impl std::error::Error for TestGateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Temper(source) => source.source(),
            Self::Exec(source) => source.source(),
            Self::NotInJob => None,
        }
    }
}

impl GateRunner for TestGates {
    type Error = TestGateError;

    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        match plugin.adapter() {
            Adapter::Temper { .. } => self.temper.run(plugin, job).map_err(TestGateError::Temper),
            Adapter::Exec { .. } => self.exec.run(plugin, job).map_err(TestGateError::Exec),
            Adapter::WeighIn { .. } => Err(TestGateError::NotInJob),
        }
    }
}
