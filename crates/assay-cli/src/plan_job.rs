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

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use assay_app::{ContractSchemaError, ContractValidator, InvalidBundle, RestFiles};
use assay_cli::outputs::{Identity, PlanOutputs};
use assay_cli::{
    ForgePullRequest, ForgeSupervisor, JobName, Needs, NeedsError, OutDir, OutDirError,
    OutputError, Outputs, PlanJob, PlanPorts, PlanRunError, RUN_FILE, RunEnv, RunEnvError,
    RunVariables, run_plan,
};
use assay_domain::ContentHash;
use assay_forge::git::{
    Authorization, CommitSha, GitEnvironment, GitError, GitLimits, HttpsRemote, InvalidHeader,
    ObjectRepo, ParseRemoteError, ParseShaError, TreeLimits,
};
use assay_forge::github::{
    Api, ForgeError, GhApi, GhEnvironment, GhError, GhSetupError, GhToken, GitHub, ThreadSleep,
};
use assay_forge::process::{GroupKiller, KillerError};

use crate::bundle;
use crate::check::{DIFF_CAP, on_path};
use crate::doctor::{Urandom, write_chain};
use crate::guard_job::{ACTIONS, open_output};

/// The one flag.
const OUT: &str = "--out";

/// A variable `assay plan` reads besides the run's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variable {
    /// `GITHUB_OUTPUT`: the file the outputs are appended to.
    Output,
    /// `ASSAY_NEEDS`: guard's outputs.
    Needs,
    /// `ASSAY_RUN_TOKEN`: the job's `github.token`.
    RunToken,
}

impl Variable {
    /// The variable's name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Output => "GITHUB_OUTPUT",
            Self::Needs => "ASSAY_NEEDS",
            Self::RunToken => "ASSAY_RUN_TOKEN",
        }
    }

    /// What it holds, for a refusal.
    const fn holds(self) -> &'static str {
        match self {
            Self::Output => "the file the job's outputs are appended to",
            Self::Needs => "guard's outputs as ${{ toJSON(needs) }} gives them",
            Self::RunToken => "the job's github.token (contents: read, pull-requests: read)",
        }
    }
}

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

/// Which pinned commit a fetch was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// `base_sha`.
    Base,
    /// `head_sha`.
    Head,
}

/// Why `assay plan` wrote no outputs; exit 2.
#[derive(Debug)]
pub(crate) enum PlanCliError {
    /// Not exactly `--out <dir>`.
    Usage,
    /// The run variables are missing or invalid.
    Run(RunEnvError),
    /// A variable is not set.
    Missing(Variable),
    /// A variable holds something it may not; its value is never shown.
    Invalid(Variable),
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// guard's outputs were refused.
    Needs(NeedsError),
    /// `--out` was refused.
    Out(OutDirError),
    /// The embedded prompt bundle was refused.
    Bundle(InvalidBundle),
    /// The contract schemas did not compile.
    Validator(ContractSchemaError),
    /// No `gh` on `PATH`.
    NoGh,
    /// No process-group killer for `gh` and git.
    Killer(KillerError),
    /// gh's scratch configuration could not be prepared.
    Gh(GhSetupError),
    /// GitHub's file list was refused or could not be read.
    Files(ForgeError<GhError>),
    /// The run token makes no git header.
    Authorization(InvalidHeader),
    /// The repository makes no https remote.
    Remote(ParseRemoteError),
    /// A pinned commit is not one git accepts.
    Commit(ParseShaError),
    /// The scratch directory could not be made.
    Scratch(io::Error),
    /// The object-only repository could not be made.
    Git(GitError),
    /// A pinned commit could not be fetched.
    Fetch { side: Side, source: GitError },
    /// The run could not be planned or its document written.
    Plan(Box<PlanRunError<io::Error>>),
    /// An output could not be appended.
    Output(OutputError),
}

impl fmt::Display for PlanCliError {
    /// `plan: `, what failed, and the cause chain.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (context, source): (&str, &dyn Error) = match self {
            Self::Usage => return write!(f, "usage: assay plan {OUT} <dir>"),
            Self::Missing(variable) => {
                let (name, holds) = (variable.name(), variable.holds());
                return write!(f, "plan: {name} is not set: it holds {holds}");
            }
            Self::Invalid(variable) => {
                let (name, holds) = (variable.name(), variable.holds());
                return write!(f, "plan: {name} is not {holds}");
            }
            Self::NoGh => {
                return f.write_str("plan: no gh on PATH: plan reads the PR's files through gh");
            }
            Self::OutputFile(source) => {
                return write!(f, "plan: opening {}: {source}", Variable::Output.name());
            }
            Self::Needs(source) => {
                write!(f, "plan: {}: ", Variable::Needs.name())?;
                return write_chain(f, source);
            }
            Self::Run(source) => ("", source),
            Self::Out(source) => ("", source),
            Self::Bundle(source) => ("the embedded prompt bundle: ", source),
            Self::Validator(source) => ("", source),
            Self::Killer(source) => ("", source),
            Self::Gh(source) => ("", source),
            Self::Files(source) => ("the PR's file list: ", source),
            Self::Authorization(source) => ("", source),
            Self::Remote(source) => ("the repository's remote: ", source),
            Self::Commit(source) => ("a pinned commit: ", source),
            Self::Scratch(source) => ("the scratch directory: ", source),
            Self::Git(source) => ("the object-only repository: ", source),
            Self::Fetch { side, source } => match side {
                Side::Base => ("fetching the base commit: ", source),
                Side::Head => ("fetching the head commit: ", source),
            },
            Self::Plan(source) => ("", source.as_ref()),
            Self::Output(source) => ("", source),
        };
        write!(f, "plan: {context}")?;
        write_chain(f, source)
    }
}

/// A scratch directory that could not be removed.
#[derive(Debug)]
pub(crate) enum Left {
    /// gh's scratch configuration directory.
    Gh(io::Error),
    /// The object-only repository's scratch directory.
    Scratch(io::Error),
}

impl fmt::Display for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gh(source) => {
                write!(
                    f,
                    "plan: could not remove gh's scratch configuration directory: {source}"
                )
            }
            Self::Scratch(source) => {
                write!(
                    f,
                    "plan: could not remove plan's scratch directory: {source}"
                )
            }
        }
    }
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
