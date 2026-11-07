//! `gates-static` (Z2s, no PR-controlled execution; 03 §2.1 steps 1-4, ADR-12): the temper
//! adapter's Z2s checks, reading the PR's base and head from a hardened object-only repository,
//! and the Weigh-In adapter's pinned `weighin ci-check`.
//!
//! The pinned commits are fetched as `gate/fetch.rs` says, with the run token only. The pinned
//! temper copy is `--temper`, prepared by the workflow from the base's `oci:sha256:` pin with
//! fetch-by-digest's `plugin.temper` selector (D8 A), and is run by `--node`; both are absolute
//! and outside the head tree. The pinned `weighin` is the optional `--weighin` (#372), also
//! absolute and outside the head tree: when it is given and base config holds a Weigh-In
//! plugin, the head commit is written as a reading checkout into the job's scratch
//! (`ObjectRepo::reading_checkout`: head's files, history reaching the base) and `ci-check`
//! runs there, with its `--store` in the scratch too. Without `--weighin` a Weigh-In gate is
//! `gate-error:not-run`; nothing else changes. Weigh-In 2.0 keeps its journals in
//! `refs/redkiln/journal/*`, which this job does not fetch yet.

use std::fmt;
use std::path::PathBuf;

use assay_app::{
    Adapter, ContractValidator, GateError, GateJob, GateJobReport, GateRun, GateRunner, Plugin,
    PluginsConfig, run_gates,
};
use assay_cli::{ForgeRevisions, ForgeSupervisor};
use assay_forge::git::{GitError, TreeLimits};
use assay_forge::process::{GroupKiller, ProcessError};
use assay_gates::{
    CiCheck, GateEnvironment, TemperAdapter, TemperError, TemperPaths, TemperSettings,
    WeighInAdapter, WeighInError, WeighInSettings,
};
use tempfile::TempDir;

use super::fetch::{Fetched, PinnedVariables, Source, commit_id};
use super::paths::Programs;
use super::{
    Finished, GateCliError, JobInputs, Paths, PinnedError, Prepared, Ran, Variable, closing,
    finish, prepare, scratch,
};

/// The reading checkout `ci-check` runs in, under the job's scratch.
const WEIGH_IN_TREE: &str = "weigh-in";
/// The ledger file `ci-check` is pointed at, under the job's scratch; it need not exist.
const WEIGH_IN_STORE: &str = "weigh-in-store/ledger.jsonl";

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
    // The temper and Weigh-In settings each store the run key.
    let settings = TemperSettings::new(paths, &environment, job.run_key.clone())
        .map_err(GateCliError::TemperSettings)?;
    let weighin = programs
        .weighin
        .filter(|_| has_weigh_in(&config))
        .map(|program| WeighInInputs {
            program,
            environment,
            run_key: job.run_key,
        });
    let scratch = scratch()?;
    let inputs = Inputs {
        source,
        killer,
        settings,
        weighin,
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

/// Whether base config holds a Weigh-In plugin, for which the reading checkout is written.
fn has_weigh_in(config: &PluginsConfig) -> bool {
    config
        .plugins()
        .iter()
        .any(|plugin| matches!(plugin.adapter(), Adapter::WeighIn { .. }))
}

/// What the Weigh-In gate needs besides the fetched commits: the pinned `weighin`, resolved
/// outside the head tree.
struct WeighInInputs {
    program: PathBuf,
    environment: GateEnvironment,
    run_key: String,
}

/// What the gates need once the job's refusals are behind it.
struct Inputs {
    source: Source,
    killer: GroupKiller,
    settings: TemperSettings,
    /// `None` without `--weighin`, or when base config holds no Weigh-In plugin.
    weighin: Option<WeighInInputs>,
}

/// Fetches the pinned commits into an object-only repository under `scratch`, writes the
/// Weigh-In gate's reading checkout when it runs, and runs the gates over them.
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
        weighin,
    } = inputs;
    // git and each pinned tool's supervisor keep a handle on the killer (a program path).
    let Fetched { repo, base, head } =
        source.fetch(scratch.path().join("objects.git"), killer.clone())?;
    let weigh_in = weighin
        .map(|weighin| {
            let tree = scratch.path().join(WEIGH_IN_TREE);
            repo.reading_checkout(&head, &tree)
                .map_err(PinnedError::WeighInCheckout)?;
            let check = CiCheck {
                program: weighin.program,
                tree,
                store: scratch.path().join(WEIGH_IN_STORE),
                base: commit_id(&base, Variable::BaseSha)?,
                head: commit_id(&head, Variable::HeadSha)?,
            };
            let settings = WeighInSettings::new(check, &weighin.environment, weighin.run_key)
                .map_err(PinnedError::WeighInSettings)?;
            Ok::<_, PinnedError>(WeighInAdapter::new(
                ForgeSupervisor::new(killer.clone()),
                settings,
            ))
        })
        .transpose()?;
    let revisions = ForgeRevisions::new(&repo, base, head, TreeLimits::default());
    let mut runner = StaticGates {
        temper: TemperAdapter::new(ForgeSupervisor::new(killer), &revisions, settings),
        weigh_in,
    };
    Ok(run_gates(config, STATIC, &mut runner, validator))
}

/// The gates `gates-static` runs: temper's Z2s checks over the PR's revisions, and Weigh-In's
/// pinned `ci-check` when `--weighin` was given.
struct StaticGates<'r> {
    temper: TemperAdapter<ForgeSupervisor, &'r ForgeRevisions<'r>>,
    weigh_in: Option<WeighInAdapter<ForgeSupervisor>>,
}

/// Why a gate of `gates-static` did not run.
#[derive(Debug)]
enum StaticGateError {
    Temper(TemperError<ProcessError, GitError>),
    WeighIn(WeighInError<ProcessError>),
    /// A Weigh-In plugin, and the job was not given `--weighin` to run it with.
    NoWeighIn,
    /// An `exec` plugin, which has no work in `gates-static`.
    NotInJob,
}

impl fmt::Display for StaticGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Temper(source) => write!(f, "{source}"),
            Self::WeighIn(source) => write!(f, "{source}"),
            Self::NoWeighIn => f.write_str(
                "the job was not given --weighin, the pinned weighin the Weigh-In gate runs",
            ),
            Self::NotInJob => f.write_str("the plugin has no work in gates-static"),
        }
    }
}

impl std::error::Error for StaticGateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Temper(source) => source.source(),
            Self::WeighIn(source) => source.source(),
            Self::NoWeighIn | Self::NotInJob => None,
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
            Adapter::WeighIn { .. } => match &mut self.weigh_in {
                Some(weigh_in) => weigh_in.run(plugin, job).map_err(StaticGateError::WeighIn),
                None => Err(StaticGateError::NoWeighIn),
            },
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
            StaticGateError::WeighIn(_)
            | StaticGateError::NoWeighIn
            | StaticGateError::NotInJob => GateError::NotRun,
        }
    }
}
