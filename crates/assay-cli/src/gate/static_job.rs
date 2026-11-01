//! `gates-static` (Z2s, no PR-controlled execution; 03 §2.1 steps 1-4, ADR-12): the temper
//! adapter's Z2s checks and the Weigh-In adapter, both reading the PR's base and head from a
//! hardened object-only repository.
//!
//! The pinned commits are fetched as `gate/fetch.rs` says, with the run token only. The pinned
//! temper copy is `--temper`, prepared by the workflow from the base's `oci:sha256:` pin with
//! fetch-by-digest's `plugin.temper` selector (D8 A), and is run by `--node`; both are absolute
//! and outside the head tree.

use std::fmt;

use assay_app::{
    Adapter, ContractValidator, GateError, GateJob, GateJobReport, GateRun, GateRunner, Plugin,
    PluginsConfig, run_gates,
};
use assay_cli::{ForgeRevisions, ForgeSupervisor};
use assay_forge::git::{GitError, TreeLimits};
use assay_forge::process::{GroupKiller, ProcessError};
use assay_gates::{
    GateEnvironment, TemperAdapter, TemperError, TemperPaths, TemperSettings, WeighInAdapter,
    WeighInError,
};
use tempfile::TempDir;

use super::fetch::{Fetched, PinnedVariables, Source};
use super::paths::Programs;
use super::{
    Finished, GateCliError, JobInputs, Paths, PinnedError, Prepared, Ran, closing, finish, prepare,
    scratch,
};

/// The job.
const STATIC: GateJob = GateJob::Static;

/// Runs `gates-static` over `paths` with `programs`, and writes its documents and output.
///
/// # Errors
///
/// [`GateCliError`] as [`super::run`] says; a pinned commit that cannot be fetched is
/// [`PinnedError::Fetch`].
pub(super) fn run(
    paths: &Paths,
    programs: Programs,
    variables: PinnedVariables,
    job: JobInputs,
) -> Result<Ran, GateCliError> {
    let source = Source::read(variables)?;
    let Prepared {
        placed,
        output,
        config,
        validator,
        killer,
    } = prepare(paths, &job.output)?;
    let programs = programs.outside(&placed.head)?;
    let environment = GateEnvironment::from_vars(job.vars);
    let paths = TemperPaths {
        node: programs.node,
        pinned: programs.temper,
        head_tree: placed.head,
    };
    // The temper settings and the Weigh-In adapter each store the run key.
    let settings = TemperSettings::new(paths, &environment, job.run_key.clone())
        .map_err(GateCliError::TemperSettings)?;
    let scratch = scratch()?;
    let inputs = Inputs {
        source,
        killer,
        settings,
        run_key: job.run_key,
    };
    let ended = gates_in(&scratch, inputs, &config, &validator).and_then(|reports| {
        let finished = Finished {
            job: STATIC,
            config: &config,
            out: &placed.out,
            output,
        };
        finish(finished, reports)
    });
    closing(scratch, ended)
}

/// What the gates need once the job's refusals are behind it.
struct Inputs {
    source: Source,
    killer: GroupKiller,
    settings: TemperSettings,
    run_key: String,
}

/// Fetches the pinned commits into an object-only repository under `scratch` and runs the
/// gates over them.
fn gates_in(
    scratch: &TempDir,
    inputs: Inputs,
    config: &PluginsConfig,
    validator: &ContractValidator,
) -> Result<Vec<GateJobReport>, GateCliError> {
    let Inputs {
        source,
        killer,
        settings,
        run_key,
    } = inputs;
    // git and the scan's supervisor each keep a handle on the killer (a program path).
    let Fetched { repo, base, head } =
        source.fetch(scratch.path().join("objects.git"), killer.clone())?;
    let revisions = ForgeRevisions::new(&repo, base, head, TreeLimits::default());
    let mut runner = StaticGates {
        temper: TemperAdapter::new(ForgeSupervisor::new(killer), &revisions, settings),
        weigh_in: WeighInAdapter::new(&revisions, run_key).map_err(PinnedError::WeighInSettings)?,
    };
    Ok(run_gates(config, STATIC, &mut runner, validator))
}

/// The gates `gates-static` runs: temper's Z2s checks and Weigh-In, over the PR's revisions.
struct StaticGates<'r> {
    temper: TemperAdapter<ForgeSupervisor, &'r ForgeRevisions<'r>>,
    weigh_in: WeighInAdapter<&'r ForgeRevisions<'r>>,
}

/// Why a gate of `gates-static` did not run.
#[derive(Debug)]
enum StaticGateError {
    Temper(TemperError<ProcessError, GitError>),
    WeighIn(WeighInError<GitError>),
    /// An `exec` plugin, which has no work in `gates-static`.
    NotInJob,
}

impl fmt::Display for StaticGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Temper(source) => write!(f, "{source}"),
            Self::WeighIn(source) => write!(f, "{source}"),
            Self::NotInJob => f.write_str("the plugin has no work in gates-static"),
        }
    }
}

impl std::error::Error for StaticGateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Temper(source) => source.source(),
            Self::WeighIn(source) => source.source(),
            Self::NotInJob => None,
        }
    }
}

impl<'r> GateRunner for StaticGates<'r> {
    type Error = StaticGateError;

    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        match plugin.adapter() {
            Adapter::Temper { .. } => self
                .temper
                .run(plugin, job)
                .map_err(StaticGateError::Temper),
            Adapter::WeighIn { .. } => self
                .weigh_in
                .run(plugin, job)
                .map_err(StaticGateError::WeighIn),
            Adapter::Exec { .. } => Err(StaticGateError::NotInJob),
        }
    }

    /// The temper adapter's own reading of its error; [`GateError::NotRun`] otherwise.
    fn gate_error(error: &Self::Error) -> GateError {
        match error {
            StaticGateError::Temper(source) => <TemperAdapter<
                ForgeSupervisor,
                &'r ForgeRevisions<'r>,
            > as GateRunner>::gate_error(source),
            StaticGateError::WeighIn(_) | StaticGateError::NotInJob => GateError::NotRun,
        }
    }
}
