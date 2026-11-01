//! `gates-static` (Z2s, no PR-controlled execution; 03 §2.1 steps 1-4, ADR-12): the temper
//! adapter's Z2s checks and the Weigh-In adapter, both reading the PR's base and head from a
//! hardened object-only repository.
//!
//! The pinned commits come from `ASSAY_BASE_SHA` and `ASSAY_HEAD_SHA` (guard's outputs), and are
//! fetched by SHA into a repository in the job's scratch directory from
//! `https://github.com/$GITHUB_REPOSITORY.git`, with an authorized git fetch whose header is
//! `ASSAY_RUN_TOKEN`'s (the job's `github.token`) and no other token's. The token reaches no gate:
//! the gate environment's allowlist drops it. The pinned temper copy is `--temper`, prepared by
//! the workflow from the base's `oci:sha256:` pin with fetch-by-digest's `plugin.temper` selector
//! (D8 A), and is run by `--node`; both are absolute and outside the head tree.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use assay_app::{Adapter, GateError, GateJob, GateRun, GateRunner, Plugin, run_gates};
use assay_cli::{ForgeRevisions, ForgeSupervisor};
use assay_forge::git::{
    Authorization, CommitSha, GitEnvironment, GitError, GitLimits, HttpsRemote, ObjectRepo,
    TreeLimits,
};
use assay_forge::github::{GhToken, Repository};
use assay_forge::process::{GroupKiller, ProcessError};
use assay_gates::{
    GateEnvironment, TemperAdapter, TemperError, TemperPaths, TemperSettings, WeighInAdapter,
    WeighInError,
};

use super::paths::Programs;
use super::{
    Finished, GateCliError, JobInputs, Paths, Prepared, Ran, Side, StaticError, Variable, finish,
    prepare, scratch,
};

/// The job.
const STATIC: GateJob = GateJob::Static;

/// What the composition root read of the process for the fetch (ARC-11).
#[derive(Debug)]
pub(crate) struct StaticVariables {
    /// [`Variable::BaseSha`].
    pub(crate) base_sha: Option<OsString>,
    /// [`Variable::HeadSha`].
    pub(crate) head_sha: Option<OsString>,
    /// [`Variable::RunToken`].
    pub(crate) token: Option<OsString>,
    /// [`Variable::Repository`].
    pub(crate) repository: Option<OsString>,
    /// [`Variable::Path`].
    pub(crate) path: Option<OsString>,
}

/// Where the pinned commits are fetched from, and how.
struct Source {
    base: CommitSha,
    head: CommitSha,
    remote: HttpsRemote,
    /// The run token as git's header, sent to `remote` only.
    authorization: Authorization,
    /// `PATH`, the only variable git inherits.
    path: OsString,
}

impl Source {
    /// Reads the fetch's variables, each refused by name and never shown.
    fn read(variables: StaticVariables) -> Result<Self, StaticError> {
        let base = commit(variables.base_sha, Variable::BaseSha)?;
        let head = commit(variables.head_sha, Variable::HeadSha)?;
        let authorization = present(variables.token, Variable::RunToken)?
            .to_str()
            .and_then(|text| GhToken::new(text).ok())
            .ok_or(StaticError::Invalid(Variable::RunToken))?
            .git_authorization()
            .map_err(StaticError::Authorization)?;
        let repo: Repository = present(variables.repository, Variable::Repository)?
            .to_str()
            .and_then(|text| text.parse().ok())
            .ok_or(StaticError::Invalid(Variable::Repository))?;
        let remote = format!("https://github.com/{repo}.git")
            .parse()
            .map_err(StaticError::Remote)?;
        let path = present(variables.path, Variable::Path)?;
        Ok(Self {
            base,
            head,
            remote,
            authorization,
            path,
        })
    }

    /// Fetches the base and then the head commit, by SHA, into an object-only repository at
    /// `git_dir`, whose git processes `killer` stops.
    fn fetch(self, git_dir: PathBuf, killer: GroupKiller) -> Result<Fetched, StaticError> {
        let git = GitEnvironment::new(self.path, killer);
        let repo =
            ObjectRepo::init(git_dir, git, GitLimits::default()).map_err(StaticError::Git)?;
        for (side, commit) in [(Side::Base, &self.base), (Side::Head, &self.head)] {
            repo.fetch_authorized(&self.remote, &self.authorization, commit)
                .map_err(|source| StaticError::Fetch { side, source })?;
        }
        Ok(Fetched {
            repo,
            base: self.base,
            head: self.head,
        })
    }
}

/// The object-only repository holding both pinned commits.
struct Fetched {
    repo: ObjectRepo,
    base: CommitSha,
    head: CommitSha,
}

/// `value`, which must be set.
fn present(value: Option<OsString>, variable: Variable) -> Result<OsString, StaticError> {
    value.ok_or(StaticError::Missing(variable))
}

/// The commit `value` names: 40 lowercase hex digits.
fn commit(value: Option<OsString>, variable: Variable) -> Result<CommitSha, StaticError> {
    present(value, variable)?
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or(StaticError::Invalid(variable))
}

/// Runs `gates-static` over `paths` with `programs`, and writes its documents and output.
///
/// # Errors
///
/// [`GateCliError`] as [`super::run`] says; a pinned commit that cannot be fetched is
/// [`StaticError::Fetch`].
pub(super) fn run(
    paths: &Paths,
    programs: Programs,
    variables: StaticVariables,
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
    // git and the scan's supervisor each keep a handle on the killer (a program path).
    let Fetched { repo, base, head } =
        source.fetch(scratch.path().join("objects.git"), killer.clone())?;
    let revisions = ForgeRevisions::new(&repo, base, head, TreeLimits::default());
    let mut runner = StaticGates {
        temper: TemperAdapter::new(ForgeSupervisor::new(killer), &revisions, settings),
        weigh_in: WeighInAdapter::new(&revisions, job.run_key)
            .map_err(StaticError::WeighInSettings)?,
    };
    let reports = run_gates(&config, STATIC, &mut runner, &validator);
    finish(
        Finished {
            job: STATIC,
            config: &config,
            out: &placed.out,
            output,
        },
        reports,
        scratch,
    )
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
