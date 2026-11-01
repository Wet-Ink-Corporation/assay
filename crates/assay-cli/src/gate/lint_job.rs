//! `gates-lint` (Z2b, 03 §2.1 steps 5-7): temper's declared `lint` steps, the pinned
//! `baseline --check` with its ratchet against the merge base, and the `exec` gates whose `job`
//! is `lint`, all run in the `--head` checkout with the gate environment.
//!
//! The ratchet base is `ASSAY_BASE_SHA` with `ASSAY_MERGE_BASE` (plan's `merge_base` output, D4
//! A), both 40 lowercase hex digits, refused by name before anything runs. The base and head are
//! fetched as `gate/fetch.rs` says, with the run token only, and the checkout must be exactly the
//! head commit's tree (`gate/checkout.rs`, #348) before any step, temper or `exec` gate runs,
//! since they run PR-controlled code. temper's changed-line diff runs in the checkout's own git
//! (`merge-base --base HEAD` against the working tree), so the checkout is verified rather than
//! replaced by an export, which would have no history; its merge base is then believed only when
//! it equals `ASSAY_MERGE_BASE`. Evidence is read from the object-only repository. The pinned
//! temper copy and `node` are `gates-static`'s (D8 A).

use std::fmt;
use std::fs;
use std::path::Path;

use assay_app::{
    Adapter, BuildJob, CommitId, ContractValidator, GateError, GateJob, GateJobReport, GateRun,
    GateRunner, Plugin, PluginsConfig, run_gates,
};
use assay_cli::{ForgeRevisions, ForgeSupervisor};
use assay_forge::git::{CommitSha, GitError, TreeLimits};
use assay_forge::process::{GroupKiller, ProcessError};
use assay_gates::{
    ExecAdapter, ExecError, ExecLimits, ExecSettings, GateEnvironment, RatchetBase, TemperAdapter,
    TemperError, TemperPaths, TemperSettings,
};
use tempfile::TempDir;

use super::checkout;
use super::fetch::{Fetched, PinnedVariables, Source, commit};
use super::paths::Programs;
use super::{
    Finished, GateCliError, JobInputs, Paths, PinnedError, Prepared, Ran, Variable, closing,
    finish, prepare, scratch,
};

/// The job.
pub(super) const LINT: GateJob = GateJob::Build(BuildJob::Lint);

/// Runs `gates-lint` over `paths` with `programs`, and writes its documents and output.
///
/// # Errors
///
/// [`GateCliError`] as [`super::run`] says; a merge base that is missing or not a commit id is
/// [`PinnedError::Missing`] or [`PinnedError::Invalid`] before anything runs, and a checkout
/// that is not the head commit's tree is [`PinnedError::Checkout`] before any gate runs.
pub(super) fn run(
    paths: &Paths,
    programs: Programs,
    mut variables: PinnedVariables,
    job: JobInputs,
) -> Result<Ran, GateCliError> {
    let merge_base: CommitId = commit(variables.merge_base.take(), Variable::MergeBase)?;
    let source = Source::read(variables)?;
    let ratchet = RatchetBase {
        base: commit_id(source.base(), Variable::BaseSha)?,
        merge_base,
    };
    let Prepared {
        placed,
        output,
        config,
        validator,
        killer,
    } = prepare(paths, &job.output)?;
    let programs = programs.outside(&placed.head)?;
    let environment = GateEnvironment::from_vars(job.vars);
    let temper = TemperPaths {
        node: programs.node,
        pinned: programs.temper,
        // The settings store their own copy; the checkout is still verified and run in.
        head_tree: placed.head.clone(),
    };
    let settings = TemperSettings::new(temper, &environment, job.run_key)
        .map_err(GateCliError::TemperSettings)?
        .with_ratchet(ratchet);
    let scratch = scratch()?;
    let inputs = Inputs {
        source,
        killer,
        settings,
        environment,
        checkout: &placed.head,
    };
    let ended = gates_in(&scratch, inputs, &config, &validator).and_then(|reports| {
        let finished = Finished {
            job: LINT,
            config: &config,
            out: &placed.out,
            output,
        };
        finish(finished, reports)
    });
    closing(scratch, ended)
}

/// `sha` as the app's commit id; both are 40 lowercase hex digits.
fn commit_id(sha: &CommitSha, variable: Variable) -> Result<CommitId, PinnedError> {
    sha.as_str()
        .parse()
        .map_err(|_not_an_id| PinnedError::Invalid(variable))
}

/// What the gates need once the job's refusals are behind it.
struct Inputs<'p> {
    source: Source,
    killer: GroupKiller,
    settings: TemperSettings,
    environment: GateEnvironment,
    checkout: &'p Path,
}

/// Fetches the pinned commits into an object-only repository under `scratch`, refuses a
/// checkout that is not the head's tree, and runs the gates in it.
fn gates_in(
    scratch: &TempDir,
    inputs: Inputs<'_>,
    config: &PluginsConfig,
    validator: &ContractValidator,
) -> Result<Vec<GateJobReport>, GateCliError> {
    let Inputs {
        source,
        killer,
        settings,
        environment,
        checkout,
    } = inputs;
    // git and each adapter's supervisor keep a handle on the killer (a program path).
    let Fetched { repo, base, head } =
        source.fetch(scratch.path().join("objects.git"), killer.clone())?;
    checkout::verify(&repo, &head, checkout, TreeLimits::default())?;
    let documents = scratch.path().join("exec");
    if let Err(source) = fs::create_dir(&documents) {
        return Err(GateCliError::Scratch {
            path: documents,
            source,
        });
    }
    let exec = ExecSettings::new(checkout, &documents, environment, ExecLimits::DEFAULT)
        .map_err(GateCliError::ExecSettings)?;
    let revisions = ForgeRevisions::new(&repo, base, head, TreeLimits::default());
    let mut runner = LintGates {
        temper: TemperAdapter::new(ForgeSupervisor::new(killer.clone()), &revisions, settings),
        exec: ExecAdapter::new(ForgeSupervisor::new(killer), exec),
    };
    Ok(run_gates(config, LINT, &mut runner, validator))
}

/// The gates `gates-lint` runs: temper's lint steps and ratchet, and the job's `exec` gates.
struct LintGates<'r> {
    temper: TemperAdapter<ForgeSupervisor, &'r ForgeRevisions<'r>>,
    exec: ExecAdapter<ForgeSupervisor>,
}

/// Why a gate of `gates-lint` did not run.
#[derive(Debug)]
enum LintGateError {
    Temper(TemperError<ProcessError, GitError>),
    Exec(ExecError<ProcessError>),
    /// A Weigh-In plugin, which has no work in `gates-lint`.
    NotInJob,
}

impl fmt::Display for LintGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Temper(source) => write!(f, "{source}"),
            Self::Exec(source) => write!(f, "{source}"),
            Self::NotInJob => f.write_str("the plugin has no work in gates-lint"),
        }
    }
}

impl std::error::Error for LintGateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Temper(source) => source.source(),
            Self::Exec(source) => source.source(),
            Self::NotInJob => None,
        }
    }
}

impl<'r> GateRunner for LintGates<'r> {
    type Error = LintGateError;

    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        match plugin.adapter() {
            Adapter::Temper { .. } => self.temper.run(plugin, job).map_err(LintGateError::Temper),
            Adapter::Exec { .. } => self.exec.run(plugin, job).map_err(LintGateError::Exec),
            Adapter::WeighIn { .. } => Err(LintGateError::NotInJob),
        }
    }

    /// The temper adapter's own reading of its error (a degraded ratchet is
    /// [`GateError::RatchetDegraded`]); [`GateError::NotRun`] otherwise.
    fn gate_error(error: &Self::Error) -> GateError {
        match error {
            LintGateError::Temper(source) => {
                <TemperAdapter<ForgeSupervisor, &'r ForgeRevisions<'r>> as GateRunner>::gate_error(
                    source,
                )
            }
            LintGateError::Exec(_) | LintGateError::NotInJob => GateError::NotRun,
        }
    }
}
