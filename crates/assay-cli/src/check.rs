//! `assay check --local [--base <branch>] [--claude <program>]` (#107, 02 §6): the pipeline's
//! advisory verdict on the working tree, run from the repository's root.
//!
//! The working tree is snapshotted into a scratch object-only repository that borrows the
//! developer's objects (their repository is never written), planned against
//! `refs/remotes/origin/<branch>` (default `main`), gated (`exec` gates run in the working tree;
//! temper and Weigh-In do not run locally), reviewed by the logged-in interactive `claude` on a
//! checkout of the snapshot, and decided. The report goes to `<git dir>/assay/<tree>.json` and
//! the terminal. It is advisory: CI never reads it, and no contract accepts it.
//!
//! Exit 0 when the decision is `success` or `neutral`, 1 otherwise, 2 when the check cannot run.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{
    Adapter, BundleFiles, ChangedFiles, CheckError, CheckPorts, CheckRequest, CommitId,
    ContractSchemaError, ContractValidator, GateJob, GateRun, GateRunner, IncludeTargets,
    InvalidBundle, LocalReport, ParseCommitIdError, PlanRequest, Plugin, PromptBundle,
    PullRequestEvent, PullRequestObjects, RestFile, RestFiles, ReviewSetup, check_local,
};
use assay_cli::{ForgeObjectsError, ForgePullRequest, ForgeSupervisor};
use assay_domain::{Conclusion, ContentHash, RunUid};
use assay_forge::git::{
    CommitSha, GitEnvironment, GitError, GitLimits, ObjectRepo, TreeLimits, UserDirs, WorkingTree,
};
use assay_forge::process::{GroupKiller, KillerError};
use assay_gates::{
    ExecAdapter, ExecError, ExecLimits, ExecSettings, ExecSettingsError, GateEnvironment,
    Supervisor,
};
use assay_runner::{ClaudeHeadless, EnvVarError, HeadlessEnv, Workdir, WorkdirError};
use tempfile::{NamedTempFile, TempDir};

use crate::doctor::write_chain;

/// The directory under the git directory that holds local reports: `.git/assay`. Nothing in CI
/// reads it (`tests/it/check.rs` pins that no workflow and no other module names it).
const REPORT_DIR: &str = "assay";

/// The base branch when `--base` is not given.
const DEFAULT_BASE: &str = "main";

/// The shipped prompt bundle and rubric, versioned with this binary.
const SYSTEM_PROMPT: &str = include_str!("../../../plugin/prompts/v1/system.md");
const COMBINED: &str = include_str!("../../../plugin/prompts/v1/combined.md");
const COMBINED_MICRO: &str = include_str!("../../../plugin/prompts/v1/combined-micro.md");
const VERIFIER: &str = include_str!("../../../plugin/prompts/v1/verifier.md");
const VERIFIER_CARRIED: &str = include_str!("../../../plugin/prompts/v1/verifier-carried.md");
const COORDINATOR: &str = include_str!("../../../plugin/prompts/v1/coordinator.md");
const BUGS: &str = include_str!("../../../plugin/prompts/v1/bugs.md");
const SECURITY: &str = include_str!("../../../plugin/prompts/v1/security.md");
const DESIGN: &str = include_str!("../../../plugin/prompts/v1/design.md");
const META: &str = include_str!("../../../plugin/prompts/v1/meta.md");
const RUBRIC: &[u8] = include_bytes!("../../../rubric/v1.toml");

/// Wall-clock cap of the diff that lists the changed files.
const DIFF_CAP: Duration = Duration::from_mins(1);

/// Wall-clock cap of each review attempt.
const REVIEW_TIMEOUT: Duration = Duration::from_mins(15);

/// The placeholder repository a working tree is planned as: it names no GitHub repository.
const LOCAL_REPO: &str = "local/working-tree";

/// The environment `check` reads, gathered by `main` (ARC-11).
pub(crate) struct CheckVariables {
    /// The working directory: the repository's root.
    pub(crate) cwd: io::Result<PathBuf>,
    /// `PATH`, for git, the gates and the CLI.
    pub(crate) path: Option<OsString>,
    /// `HOME`, where the CLI finds its stored login and git the developer's user config.
    pub(crate) home: Option<OsString>,
    /// `XDG_CONFIG_HOME`, where git also looks for the developer's user config.
    pub(crate) xdg_config_home: Option<OsString>,
    /// Every variable, for the gates' allowlist.
    pub(crate) vars: Vec<(OsString, OsString)>,
}

/// Why the check could not be run.
#[derive(Debug)]
pub(crate) enum CheckCliError {
    /// The arguments are not `--local` followed by distinct known options.
    Usage,
    /// The base branch is not a plain branch name.
    BaseName(String),
    /// `refs/remotes/origin/<branch>` names no commit.
    NoBase(String),
    /// No `claude` on `PATH`, and none given.
    NoClaude,
    /// The `--claude` program is a relative path.
    RelativeProgram(PathBuf),
    /// The working directory could not be read.
    Cwd(io::Error),
    /// No process-group killer is available on this host.
    Killer(KillerError),
    /// git could not read the working tree or snapshot it.
    Git(GitError),
    /// The scratch directory could not be prepared.
    Scratch(io::Error),
    /// The shipped prompt bundle was refused.
    Bundle(InvalidBundle),
    /// The gate settings were refused.
    Settings(ExecSettingsError),
    /// The CLI's working directory was refused.
    Workdir(WorkdirError),
    /// `HOME` or `PATH` could not be passed to the CLI.
    Env(EnvVarError),
    /// A commit id git printed is not one the app accepts.
    Commit(ParseCommitIdError),
    /// The changed files could not be listed.
    Objects(ForgeObjectsError),
    /// The contract schemas did not compile.
    Contracts(ContractSchemaError),
    /// The check stopped.
    Check(CheckError<ForgeObjectsError, ForgeObjectsError>),
    /// The report could not be encoded.
    Encode(serde_json::Error),
    /// The report could not be written.
    Write { path: PathBuf, source: io::Error },
}

impl fmt::Display for CheckCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str(
                "usage: assay check --local [--base <branch>] [--claude <program>], run from the \
                 repository root",
            ),
            Self::BaseName(name) => write!(f, "the base {name:?} is not a plain branch name"),
            Self::NoBase(name) => write!(
                f,
                "refs/remotes/origin/{name} is not a commit: fetch it, or pass --base <branch>"
            ),
            Self::NoClaude => f.write_str("no claude on PATH: pass --claude <absolute path>"),
            Self::RelativeProgram(path) => write!(
                f,
                "the claude program {} is not an absolute path",
                path.display()
            ),
            Self::Cwd(source) => write!(f, "cannot read the working directory: {source}"),
            Self::Killer(source) => {
                f.write_str("no process-group killer: ")?;
                write_chain(f, source)
            }
            Self::Git(source) => write_chain(f, source),
            Self::Scratch(source) => write!(f, "cannot prepare a scratch directory: {source}"),
            Self::Bundle(source) => write_chain(f, source),
            Self::Settings(source) => write_chain(f, source),
            Self::Workdir(source) => write_chain(f, source),
            Self::Env(source) => write_chain(f, source),
            Self::Commit(source) => write_chain(f, source),
            Self::Objects(source) => write_chain(f, source),
            Self::Contracts(source) => write_chain(f, source),
            Self::Check(source) => write_chain(f, source),
            Self::Encode(source) => write!(f, "cannot encode the report: {source}"),
            Self::Write { path, source } => {
                write!(f, "cannot write {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for CheckCliError {}

/// What the check produced: the report, where it was written, and the scratch directory if it
/// could not be removed.
pub(crate) struct Checked {
    pub(crate) report: LocalReport,
    pub(crate) path: PathBuf,
    pub(crate) scratch_left: Option<ScratchLeft>,
}

/// The scratch directory (a 0700 copy of the snapshot) could not be removed.
#[derive(Debug)]
pub(crate) struct ScratchLeft {
    path: PathBuf,
    source: io::Error,
}

impl fmt::Display for ScratchLeft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not remove the scratch directory {}, which holds a copy of the snapshot: {}",
            self.path.display(),
            self.source
        )
    }
}

impl Checked {
    /// Whether the decision lets the change merge.
    pub(crate) const fn passed(&self) -> bool {
        matches!(
            self.report.decision().conclusion,
            Conclusion::Success | Conclusion::Neutral
        )
    }

    /// Writes the terminal report and the report file's path.
    pub(crate) fn write(&self, mut out: impl Write) -> io::Result<()> {
        out.write_all(self.report.render().as_bytes())?;
        writeln!(out, "report: {}", self.path.display())?;
        out.flush()
    }
}

/// The parsed arguments.
struct Args {
    base: String,
    claude: Option<PathBuf>,
}

/// `--local`, then `--base <branch>` and `--claude <program>` at most once each.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, CheckCliError> {
    if args.next().as_deref() != Some("--local".as_ref()) {
        return Err(CheckCliError::Usage);
    }
    let (mut base, mut claude) = (None, None);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(CheckCliError::Usage)?;
        let slot = match flag.to_str() {
            Some("--base") if base.is_none() => &mut base,
            Some("--claude") if claude.is_none() => &mut claude,
            _ => return Err(CheckCliError::Usage),
        };
        *slot = Some(value);
    }
    let base = match base {
        None => DEFAULT_BASE.to_owned(),
        Some(base) => base
            .into_string()
            .map_err(|base| CheckCliError::BaseName(base.to_string_lossy().into_owned()))?,
    };
    if !is_branch_name(&base) {
        return Err(CheckCliError::BaseName(base));
    }
    Ok(Args {
        base,
        claude: claude.map(PathBuf::from),
    })
}

/// Whether `name` is a plain branch name: segments of ASCII letters, digits, `.`, `_` and `-`
/// joined by `/`, none empty or starting with `.` or `-`, so it can name no other revision.
fn is_branch_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.starts_with(['.', '-'])
                && !segment.contains("..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

/// The `claude` to run: `--claude`, which must be absolute, or the first `claude` on `PATH`.
fn claude_program(
    given: Option<PathBuf>,
    path: Option<&OsString>,
) -> Result<PathBuf, CheckCliError> {
    if let Some(program) = given {
        return if program.is_absolute() {
            Ok(program)
        } else {
            Err(CheckCliError::RelativeProgram(program))
        };
    }
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("claude"))
        .find(|candidate| candidate.is_file())
        .ok_or(CheckCliError::NoClaude)
}

/// Runs the check on the working tree at the working directory and writes its report.
pub(crate) fn run(
    args: impl Iterator<Item = OsString>,
    variables: CheckVariables,
) -> Result<Checked, CheckCliError> {
    let args = parse(args)?;
    let program = claude_program(args.claude, variables.path.as_ref())?;
    let root = variables.cwd.map_err(CheckCliError::Cwd)?;
    let killer = GroupKiller::system().map_err(CheckCliError::Killer)?;
    // git and the CLI each own a `PATH`. Without one git is not found, and opening the working
    // tree fails with that error.
    let path = variables.path.clone().unwrap_or_default();
    // A cheap handle (the `kill` program's path), owned by each process runner.
    let git = GitEnvironment::new(path, killer.clone());
    let limits = GitLimits::default();
    // The user dirs own their copy of `HOME`; the CLI's environment below owns its own.
    let user = UserDirs::new(variables.home.clone(), variables.xdg_config_home);
    // The working tree and the scratch repository each own an environment.
    let worktree =
        WorkingTree::open(root, git.clone(), &user, limits).map_err(CheckCliError::Git)?;
    let head = worktree.resolve("HEAD").map_err(CheckCliError::Git)?;
    let base = match worktree.resolve(&["refs/remotes/origin/", &args.base].concat()) {
        Ok(base) => base,
        Err(GitError::UnknownRevision { .. }) => return Err(CheckCliError::NoBase(args.base)),
        Err(error) => return Err(CheckCliError::Git(error)),
    };
    let scratch = Scratch::new()?;
    let repo =
        ObjectRepo::init(scratch.path("objects"), git, limits).map_err(CheckCliError::Git)?;
    let snapshot = repo
        .snapshot(&worktree, &head)
        .map_err(CheckCliError::Git)?;
    repo.checkout(&snapshot, &scratch.path("tree"))
        .map_err(CheckCliError::Git)?;
    let (bundle, system_prompt_file) = scratch.bundle()?;
    let objects = ForgePullRequest::new(&repo, TreeLimits::default(), DIFF_CAP);
    let commits = (&base, snapshot.commit());
    let plan = plan_request(&objects, commits, args.base, bundle.hash())?;
    let tree_dir = scratch.path("tree");
    let request = CheckRequest {
        plan: &plan,
        tree: snapshot.tree(),
        rubric: RUBRIC,
        review: ReviewSetup {
            bundle: &bundle,
            system_prompt_file: &system_prompt_file,
            tree_dir: &tree_dir,
            timeout: REVIEW_TIMEOUT,
        },
    };
    // As above: each runner owns its handle on the killer.
    let environment = gate_environment(&variables.vars, &plan.pull_request);
    let mut gates = scratch.gates(worktree.root(), environment, killer.clone())?;
    let env = logged_in(variables.home, variables.path)?;
    let model = scratch.model(program, env, killer)?;
    let validator = ContractValidator::new().map_err(CheckCliError::Contracts)?;
    let ports = CheckPorts {
        objects: &objects,
        patches: &objects,
        gates: &mut gates,
        model: &model,
    };
    let report = check_local(&request, ports, &validator).map_err(CheckCliError::Check)?;
    let path = write_report(&worktree.git_dir().join(REPORT_DIR), &report)?;
    Ok(Checked {
        report,
        path,
        scratch_left: scratch.close(),
    })
}

/// The working tree as a PR from `commits` (base, then the snapshot) into `branch`, with git's
/// own file list as the REST list.
fn plan_request(
    objects: &ForgePullRequest<'_>,
    (base, head): (&CommitSha, &CommitSha),
    branch: String,
    prompt_bundle: ContentHash,
) -> Result<PlanRequest, CheckCliError> {
    let event = PullRequestEvent {
        repo: LOCAL_REPO.to_owned(),
        number: NonZeroU64::MIN,
        base_sha: commit_id(base.as_str())?,
        head_sha: commit_id(head.as_str())?,
        base_ref: branch,
        draft: false,
    };
    Ok(PlanRequest {
        rest_files: git_file_list(objects, &event)?,
        pull_request: event,
        run_uid: RunUid {
            repo_id: NonZeroU64::MIN,
            run_id: NonZeroU64::MIN,
            run_attempt: NonZeroU64::MIN,
        },
        // Local mode computes no package sets (#65's cargo metadata), which is exec surface,
        // and scans no include targets.
        base_packages: None,
        head_packages: None,
        include_targets: IncludeTargets::NotScanned,
        prompt_bundle,
    })
}

/// The gate allowlist of `vars` (03 §2), with the run's commits and run key set.
fn gate_environment(vars: &[(OsString, OsString)], event: &PullRequestEvent) -> GateEnvironment {
    let run = [
        ("ASSAY_BASE_SHA", event.base_sha.as_str()),
        ("ASSAY_HEAD_SHA", event.head_sha.as_str()),
        ("ASSAY_RUN_KEY", "local"),
    ];
    let inherited = vars
        .iter()
        .filter(|(name, _)| !run.iter().any(|(set, _)| name == set))
        // The gate environment owns its copy of each variable it keeps.
        .cloned();
    GateEnvironment::from_vars(
        inherited.chain(run.map(|(name, value)| (name.into(), value.into()))),
    )
}

/// forge's commit id as the app's.
fn commit_id(sha: &str) -> Result<CommitId, CheckCliError> {
    sha.parse().map_err(CheckCliError::Commit)
}

/// git's own file list as the REST list `plan` confirms it against: a working tree has no PR.
fn git_file_list(
    objects: &ForgePullRequest<'_>,
    event: &PullRequestEvent,
) -> Result<RestFiles, CheckCliError> {
    let listed = objects
        .changed_files(&event.base_sha, &event.head_sha)
        .map_err(CheckCliError::Objects)?;
    let files: Vec<RestFile> = match listed {
        ChangedFiles::Listed(files) => files
            .into_iter()
            .map(|file| RestFile {
                path: file.path,
                previous_path: file.renamed_from,
            })
            .collect(),
        // `plan` lists again and refuses the change as too slow.
        ChangedFiles::TooSlow => Vec::new(),
    };
    Ok(RestFiles {
        changed_files: u64::try_from(files.len()).unwrap_or(u64::MAX),
        files,
    })
}

/// Writes `report` to `<dir>/<tree>.json` through a temporary file in `dir`, so a reader never
/// sees half a report.
fn write_report(dir: &Path, report: &LocalReport) -> Result<PathBuf, CheckCliError> {
    let path = dir.join([report.tree(), ".json"].concat());
    let failed = |source| CheckCliError::Write {
        // The error names the file it could not write.
        path: path.clone(),
        source,
    };
    let json = report.to_json().map_err(CheckCliError::Encode)?;
    fs::create_dir_all(dir).map_err(failed)?;
    let mut file = NamedTempFile::new_in(dir).map_err(failed)?;
    file.write_all(&json).map_err(failed)?;
    file.persist(&path).map_err(|error| failed(error.error))?;
    Ok(path)
}

/// The scratch directory: the snapshot's repository and checkout, the CLI's empty working
/// directory, the gates' output and the system prompt. Removed when dropped.
struct Scratch {
    dir: TempDir,
}

impl Scratch {
    fn new() -> Result<Self, CheckCliError> {
        Ok(Self {
            dir: tempfile::tempdir().map_err(CheckCliError::Scratch)?,
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Removes the directory, saying where it is left when it cannot be removed.
    fn close(self) -> Option<ScratchLeft> {
        let path = self.dir.path().to_path_buf();
        self.dir
            .close()
            .err()
            .map(|source| ScratchLeft { path, source })
    }

    /// The `exec` gates, run in the working tree at `root` with `environment`.
    fn gates(
        &self,
        root: &Path,
        environment: GateEnvironment,
        killer: GroupKiller,
    ) -> Result<LocalGates<ForgeSupervisor>, CheckCliError> {
        let output = self.path("gates");
        fs::create_dir(&output).map_err(CheckCliError::Scratch)?;
        let settings = ExecSettings::new(root, output, environment, ExecLimits::DEFAULT)
            .map_err(CheckCliError::Settings)?;
        Ok(LocalGates {
            exec: ExecAdapter::new(ForgeSupervisor::new(killer), settings),
        })
    }

    /// The shipped prompt bundle, and its system prompt written out for the CLI.
    fn bundle(&self) -> Result<(PromptBundle, PathBuf), CheckCliError> {
        let bundle = PromptBundle::new(BundleFiles {
            system: SYSTEM_PROMPT.to_owned(),
            // The specialists', verifier's and coordinator's instructions are part of the bundle's
            // hash, though local mode runs none of them.
            bugs: BUGS.to_owned(),
            security: SECURITY.to_owned(),
            design: DESIGN.to_owned(),
            meta: META.to_owned(),
            combined: COMBINED.to_owned(),
            combined_micro: COMBINED_MICRO.to_owned(),
            verifier: VERIFIER.to_owned(),
            verifier_carried: VERIFIER_CARRIED.to_owned(),
            coordinator: COORDINATOR.to_owned(),
        })
        .map_err(CheckCliError::Bundle)?;
        let file = self.path("system.md");
        fs::write(&file, SYSTEM_PROMPT).map_err(CheckCliError::Scratch)?;
        Ok((bundle, file))
    }

    /// `claude` at `program` with `env`, in an empty working directory.
    fn model(
        &self,
        program: PathBuf,
        env: HeadlessEnv,
        killer: GroupKiller,
    ) -> Result<ClaudeHeadless<ForgeSupervisor>, CheckCliError> {
        let empty = self.path("empty");
        fs::create_dir(&empty).map_err(CheckCliError::Scratch)?;
        let workdir = Workdir::new(empty).map_err(CheckCliError::Workdir)?;
        Ok(ClaudeHeadless::new(
            program,
            workdir,
            env,
            ForgeSupervisor::new(killer),
        ))
    }
}

/// The CLI's environment: no token, so it uses the login an interactive `claude` stored under
/// `HOME`, and `PATH`.
fn logged_in(home: Option<OsString>, path: Option<OsString>) -> Result<HeadlessEnv, CheckCliError> {
    let mut env = HeadlessEnv::logged_in();
    for (name, value) in [("HOME", home), ("PATH", path)] {
        if let Some(value) = value {
            env.pass(name, value).map_err(CheckCliError::Env)?;
        }
    }
    Ok(env)
}

/// The gates local mode runs: `exec` gates only; temper and Weigh-In need CI's sandboxes.
struct LocalGates<S> {
    exec: ExecAdapter<S>,
}

/// Why a gate did not run locally.
#[derive(Debug)]
enum LocalGateError<E> {
    /// Not an `exec` gate.
    NotLocal,
    /// The exec adapter did not run it.
    Exec(ExecError<E>),
}

impl<E: std::error::Error + 'static> fmt::Display for LocalGateError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLocal => f.write_str("not run in local mode"),
            Self::Exec(source) => write_chain(f, source),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for LocalGateError<E> {}

impl<S: Supervisor> GateRunner for LocalGates<S>
where
    S::Error: 'static,
{
    type Error = LocalGateError<S::Error>;

    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        if !matches!(plugin.adapter(), Adapter::Exec { .. }) {
            return Err(LocalGateError::NotLocal);
        }
        self.exec.run(plugin, job).map_err(LocalGateError::Exec)
    }
}
