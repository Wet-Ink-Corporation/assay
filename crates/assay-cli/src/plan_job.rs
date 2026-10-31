//! `assay plan --out <dir>` (#60 ST-5, S1, Z1): the pipeline's plan job.
//!
//! In order, everything it can refuse before the network: its one flag, the run, the
//! regular file `GITHUB_OUTPUT` names (opened, never created), guard's identity from
//! `ASSAY_NEEDS` (I2), `--out` as an [`OutDir`], `ASSAY_RUN_TOKEN` (the job's `github.token`,
//! `contents: read` and `pull-requests: read`), the embedded prompt bundle and the contract
//! schemas. Then the PR's REST file list is read through `gh`, and the pinned base and head
//! are fetched into an object-only repository in a scratch directory with an authorized git
//! fetch to `https://github.com/<repo>.git`; both authenticate with `ASSAY_RUN_TOKEN` and no
//! other token. [`run_plan`] writes `<out>/run.json`, and plan's outputs (03 §4a) are appended
//! last, so a job that could not write the document writes no output. A variable's value is
//! never shown.

mod error;

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};

use assay_app::{ContractValidator, RestFiles};
use assay_cli::outputs::{Identity, PlanOutputs};
use assay_cli::{
    ForgePullRequest, ForgeSupervisor, JobName, Needs, OutDir, Outputs, PlanJob, PlanPorts,
    RUN_FILE, RunEnv, RunVariables, run_plan,
};
use assay_domain::ContentHash;
use assay_forge::git::{
    Authorization, CommitSha, GitEnvironment, GitLimits, HttpsRemote, ObjectRepo, TreeLimits,
};
use assay_forge::github::{Api, GhApi, GhEnvironment, GhToken, GitHub, ThreadSleep};
use assay_forge::process::GroupKiller;

pub(crate) use self::error::{Left, PlanCliError, Side, Variable};
use crate::bundle;
use crate::check::{DIFF_CAP, on_path};
use crate::doctor::Urandom;
use crate::guard_job::{ACTIONS, open_output};

/// The one flag.
const OUT: &str = "--out";

/// The environment `assay plan` reads, gathered by `main` (ARC-11).
pub(crate) struct PlanVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`Variable::Output`].
    pub(crate) output: Option<OsString>,
    /// [`Variable::Needs`].
    pub(crate) needs: Option<OsString>,
    /// [`Variable::RunToken`].
    pub(crate) token: Option<OsString>,
    /// `PATH`, which finds `gh` and git.
    pub(crate) path: Option<OsString>,
    /// What `gh` inherits.
    pub(crate) gh: GhEnvironment,
}

/// How `assay plan` ended: the line for the job's log or why there is none, and what was left
/// behind.
pub(crate) struct Ended {
    /// What plan wrote, as one line.
    pub(crate) planned: Result<String, PlanCliError>,
    /// The scratch directories that could not be removed.
    pub(crate) left: Vec<Left>,
}

/// Everything plan reads before it starts a process.
struct Prepared {
    job: Job,
    /// The run token, which `gh` takes.
    token: GhToken,
    /// The same token as git's header, and where git sends it.
    fetch: (HttpsRemote, Authorization),
}

/// What the plan itself needs.
struct Job {
    run: RunEnv,
    output: File,
    identity: Identity,
    out: OutDir,
    prompt_bundle: ContentHash,
    validator: ContractValidator,
}

/// Runs `assay plan` with `args` (after `plan`) and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: PlanVariables) -> Ended {
    let mut left = Vec::new();
    let planned =
        prepare(args, &variables).and_then(|prepared| connect(prepared, variables, &mut left));
    Ended { planned, left }
}

/// `--out <dir>`, exactly.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<PathBuf, PlanCliError> {
    match (args.next(), args.next(), args.next()) {
        (Some(flag), Some(out), None) if flag == OUT => Ok(PathBuf::from(out)),
        _ => Err(PlanCliError::Usage),
    }
}

/// Everything plan can refuse before it starts a process, the arguments and the run first.
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &PlanVariables,
) -> Result<Prepared, PlanCliError> {
    let out = parse(args)?;
    let run = RunEnv::from_vars(&variables.run).map_err(PlanCliError::Run)?;
    let output = present(variables.output.as_ref(), Variable::Output)?;
    let output = open_output(output).map_err(PlanCliError::OutputFile)?;
    let needs = present(variables.needs.as_ref(), Variable::Needs)?;
    let needs =
        Needs::parse(needs.as_encoded_bytes(), &[JobName::Guard]).map_err(PlanCliError::Needs)?;
    let identity = Identity::from_needs(&needs).map_err(PlanCliError::Needs)?;
    let out = OutDir::new(&out).map_err(PlanCliError::Out)?;
    let token = present(variables.token.as_ref(), Variable::RunToken)?
        .to_str()
        .and_then(|text| GhToken::new(text).ok())
        .ok_or(PlanCliError::Invalid(Variable::RunToken))?;
    // git authenticates with the same token as gh, sent to the repository's URL only.
    let authorization = token
        .git_authorization()
        .map_err(PlanCliError::Authorization)?;
    let remote = format!("https://github.com/{}.git", run.repo)
        .parse()
        .map_err(PlanCliError::Remote)?;
    let prompt_bundle = bundle::embedded().map_err(PlanCliError::Bundle)?.hash();
    let validator = ContractValidator::new().map_err(PlanCliError::Validator)?;
    Ok(Prepared {
        job: Job {
            run,
            output,
            identity,
            out,
            prompt_bundle,
            validator,
        },
        token,
        fetch: (remote, authorization),
    })
}

/// `value`, which must be set.
fn present(value: Option<&OsString>, variable: Variable) -> Result<&OsString, PlanCliError> {
    value.ok_or(PlanCliError::Missing(variable))
}

/// Reads the file list through `gh`, fetches the pinned commits into a scratch directory and
/// plans; the directories are removed after, each one that could not be pushed onto `left`.
fn connect(
    prepared: Prepared,
    variables: PlanVariables,
    left: &mut Vec<Left>,
) -> Result<String, PlanCliError> {
    let Prepared { job, token, fetch } = prepared;
    let program = on_path("gh", variables.path.as_ref()).ok_or(PlanCliError::NoGh)?;
    let path = variables.path.ok_or(PlanCliError::NoGh)?;
    let killer = GroupKiller::system().map_err(PlanCliError::Killer)?;
    // The supervisor and the repository each keep a handle on the killer (a program path).
    let gh = GhApi::new(
        ForgeSupervisor::new(killer.clone()),
        program,
        token,
        variables.gh,
    )
    .map_err(PlanCliError::Gh)?;
    let rest_files = read_files(&job, gh, left)?;
    let scratch = tempfile::tempdir().map_err(PlanCliError::Scratch)?;
    let fetched = Fetched {
        dir: scratch.path(),
        git: GitEnvironment::new(path, killer),
        fetch,
    };
    let planned = plan_in(job, rest_files, fetched);
    if let Err(error) = scratch.close() {
        left.push(Left::Scratch(error));
    }
    planned
}

/// GitHub's file list of guard's PR, read through `gh` (authenticated as the run token); gh's
/// scratch directory is removed after.
fn read_files(
    job: &Job,
    gh: GhApi<ForgeSupervisor>,
    left: &mut Vec<Left>,
) -> Result<RestFiles, PlanCliError> {
    // The adapter stores the repository; the run keeps it for the plan request.
    let github = GitHub::new(Api::new(gh, ThreadSleep), job.run.repo.clone(), ACTIONS);
    let files = github.pull_files(job.identity.pr);
    if let Err(error) = github.into_api().into_transport().close() {
        left.push(Left::Gh(error));
    }
    files.map_err(PlanCliError::Files)
}

/// Where and how the pinned commits are fetched.
struct Fetched<'d> {
    dir: &'d Path,
    git: GitEnvironment,
    fetch: (HttpsRemote, Authorization),
}

/// Fetches the pinned commits into an object-only repository under `fetched.dir`, plans over
/// them, writes the document and then the outputs.
fn plan_in(job: Job, rest_files: RestFiles, fetched: Fetched<'_>) -> Result<String, PlanCliError> {
    let Job {
        run,
        output,
        identity,
        out,
        prompt_bundle,
        validator,
    } = job;
    let (remote, authorization) = fetched.fetch;
    let repo = ObjectRepo::init(
        fetched.dir.join("objects.git"),
        fetched.git,
        GitLimits::default(),
    )
    .map_err(PlanCliError::Git)?;
    for (side, commit) in [
        (Side::Base, &identity.base_sha),
        (Side::Head, &identity.head_sha),
    ] {
        let commit: CommitSha = commit.as_str().parse().map_err(PlanCliError::Commit)?;
        repo.fetch_authorized(&remote, &authorization, &commit)
            .map_err(|source| PlanCliError::Fetch { side, source })?;
    }
    let objects = ForgePullRequest::new(&repo, TreeLimits::default(), DIFF_CAP);
    let outputs = run_plan(
        PlanJob {
            run: &run,
            identity,
            rest_files,
            prompt_bundle,
        },
        PlanPorts {
            objects: &objects,
            nonces: &mut Urandom,
            validator: &validator,
        },
        &out,
    )
    .map_err(|error| PlanCliError::Plan(Box::new(error)))?;
    let mut writer = Outputs::new(output);
    for (name, value) in outputs.outputs() {
        writer.write(name, &value).map_err(PlanCliError::Output)?;
    }
    Ok(line(&out, &outputs))
}

/// What plan wrote, as one line for the job's log.
fn line(out: &OutDir, outputs: &PlanOutputs) -> String {
    format!(
        "assay plan: wrote {} ({}); tier {}, review {}",
        out.path().join(RUN_FILE).display(),
        outputs.run_digest(),
        outputs.tier().code(),
        outputs.review()
    )
}
