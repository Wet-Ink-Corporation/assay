//! The wiring `assay`'s ledger jobs share (#339): `open` and `release` now, `admit` next.
//!
//! Each reads `[assay].ledger_repo` from base config ([`ledger_repo`]) and the
//! `assay-ledger-writer` App's installation token ([`LEDGER_TOKEN`], through [`token`]); then
//! [`LedgerTransport::open`] builds the ledger's `gh` transport and the object-only repository,
//! in a scratch directory, that ref deletions are pushed from, and [`LedgerTransport::run`]
//! hands out the ledger's file and ref adapters and takes everything down after. Each refusal
//! is a [`LedgerSetupError`] that starts with the job's name. A variable's value is never shown.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_cli::ForgeSupervisor;
use assay_forge::git::{GitEnvironment, GitError, GitLimits, ObjectRepo, PushRemote};
use assay_forge::github::{
    Api, GhApi, GhEnvironment, GhSetupError, GhToken, LedgerRefsRepo, LedgerRemoteError,
    LedgerRepo, Repository, ThreadSleep, ledger_push_remote,
};
use assay_forge::process::{GroupKiller, KillerError};
use tempfile::TempDir;

use crate::check::on_path;
use crate::doctor::{self, DoctorCliError, write_chain};

/// The upstream jobs' outputs, `${{ toJSON(needs) }}`.
pub(crate) const NEEDS: &str = "ASSAY_NEEDS";
/// The `assay-ledger-writer` App's installation token.
pub(crate) const LEDGER_TOKEN: &str = "ASSAY_LEDGER_TOKEN";

/// A ref deletion's push: a few refs to one remote, well inside a lock's lease (H-13).
const PUSH_LIMITS: GitLimits = GitLimits {
    timeout: Duration::from_secs(60),
    max_output_bytes: 1 << 20,
};

/// `gh` through the shared supervisor.
pub(crate) type Gh = GhApi<ForgeSupervisor>;
/// The ledger's files, read and written through the ledger's transport.
pub(crate) type Files<'g> = LedgerRepo<&'g Gh, ThreadSleep>;
/// The ledger's lease refs, created and read through the ledger's transport.
pub(crate) type Refs<'g> = LedgerRefsRepo<&'g Gh, ThreadSleep>;

/// A job that writes the ledger: the name its refusals start with, and what it does there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LedgerJob {
    /// The subcommand, `open` or `release`.
    pub(crate) name: &'static str,
    /// Why it needs a ledger, as a clause: `open records the push in the ledger`.
    pub(crate) purpose: &'static str,
}

/// What is wrong with a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Problem {
    /// It is not set.
    Missing,
    /// It holds something it may not; its value is never shown.
    Invalid,
}

/// A variable's refusal: `<job>: <name> is not set: it holds <holds>`, or
/// `<job>: <name> is not <holds>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VariableRefusal<'a> {
    /// The job, as its refusals start.
    pub(crate) job: &'a str,
    /// The variable.
    pub(crate) name: &'a str,
    /// What it holds.
    pub(crate) holds: &'a str,
    /// What is wrong with it.
    pub(crate) problem: Problem,
}

impl fmt::Display for VariableRefusal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            job,
            name,
            holds,
            problem,
        } = *self;
        match problem {
            Problem::Missing => write!(f, "{job}: {name} is not set: it holds {holds}"),
            Problem::Invalid => write!(f, "{job}: {name} is not {holds}"),
        }
    }
}

/// Why a ledger job's base config or ports could not be set up.
#[derive(Debug)]
pub(crate) struct LedgerSetupError {
    job: LedgerJob,
    cause: Cause,
}

/// What [`LedgerSetupError`] stopped at.
#[derive(Debug)]
pub(crate) enum Cause {
    /// Base config could not be read or was refused.
    Config(DoctorCliError),
    /// Base config names no ledger repository.
    NoLedgerRepo,
    /// `[assay].ledger_repo` is not `owner/name`.
    LedgerRepo,
    /// No `gh` on `PATH`.
    NoGh,
    /// No process-group killer for `gh` and git.
    Killer(KillerError),
    /// gh's scratch configuration could not be prepared.
    Gh(GhSetupError),
    /// The scratch directory could not be made.
    Scratch(io::Error),
    /// The object-only repository ref deletions push from could not be made.
    Git(GitError),
    /// The ledger's push remote could not be built.
    Remote(LedgerRemoteError),
}

impl LedgerJob {
    /// `cause`, as this job's.
    const fn refused(self, cause: Cause) -> LedgerSetupError {
        LedgerSetupError { job: self, cause }
    }
}

impl fmt::Display for LedgerSetupError {
    /// `<job>: `, what failed, and the cause chain.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let LedgerJob { name, purpose } = self.job;
        let (context, source): (&str, &dyn Error) = match &self.cause {
            Cause::Config(source) => return write!(f, "{name}: {source}"),
            Cause::NoLedgerRepo => {
                return write!(
                    f,
                    "{name}: base config sets no [assay].ledger_repo: {purpose}"
                );
            }
            Cause::LedgerRepo => {
                return write!(
                    f,
                    "{name}: base config's [assay].ledger_repo is not owner/name"
                );
            }
            Cause::NoGh => {
                return write!(f, "{name}: no gh on PATH: {name} reaches GitHub through gh");
            }
            Cause::Scratch(source) => return write!(f, "{name}: the scratch directory: {source}"),
            Cause::Killer(source) => ("", source),
            Cause::Gh(source) => ("", source),
            Cause::Git(source) => ("the ledger's push repository: ", source),
            Cause::Remote(source) => ("the ledger's push remote: ", source),
        };
        write!(f, "{name}: {context}")?;
        write_chain(f, source)
    }
}

/// A scratch directory that could not be removed.
#[derive(Debug)]
pub(crate) enum Left {
    /// gh's scratch configuration directory, of the named token's transport.
    Gh(&'static str, io::Error),
    /// The named job's scratch directory, which held the ledger's push repository.
    Scratch(&'static str, io::Error),
}

impl fmt::Display for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gh(token, source) => write!(
                f,
                "could not remove gh's scratch configuration directory ({token}): {source}"
            ),
            Self::Scratch(job, source) => {
                write!(f, "could not remove {job}'s scratch directory: {source}")
            }
        }
    }
}

/// How a ledger job ended: its line for the job's log or why there is none, and what was left
/// behind.
pub(crate) struct Ended<E> {
    /// What the job did, as one line.
    pub(crate) line: Result<String, E>,
    /// The scratch directories that could not be removed.
    pub(crate) left: Vec<Left>,
}

/// `value` as a token gh may carry; its value is never shown.
pub(crate) fn token(value: Option<&OsStr>) -> Result<GhToken, Problem> {
    let text = value
        .ok_or(Problem::Missing)?
        .to_str()
        .ok_or(Problem::Invalid)?;
    GhToken::new(text).map_err(|_invalid| Problem::Invalid)
}

/// The ledger repository base config at `config` names in `[assay].ledger_repo`, for `job`.
pub(crate) fn ledger_repo(job: LedgerJob, config: &Path) -> Result<Repository, LedgerSetupError> {
    doctor::read_pins(config)
        .map_err(|source| job.refused(Cause::Config(source)))?
        .assay()
        .ledger_repo
        .as_deref()
        .ok_or_else(|| job.refused(Cause::NoLedgerRepo))?
        .parse()
        .map_err(|_not_owner_name| job.refused(Cause::LedgerRepo))
}

/// How `gh` runs, for each transport a job builds.
pub(crate) struct GhSetup {
    job: LedgerJob,
    supervisor: ForgeSupervisor,
    program: PathBuf,
    environment: GhEnvironment,
}

impl GhSetup {
    /// A transport authenticated as `token`.
    pub(crate) fn transport(&self, token: GhToken) -> Result<Gh, LedgerSetupError> {
        // Each transport stores its own handle, program and environment.
        GhApi::new(
            self.supervisor.clone(),
            self.program.clone(),
            token,
            self.environment.clone(),
        )
        .map_err(|source| self.job.refused(Cause::Gh(source)))
    }
}

/// What [`LedgerTransport::run`] hands its job.
pub(crate) struct LedgerPorts<'p, 'g> {
    /// How `gh` runs, for a transport of another token.
    pub(crate) gh: &'p GhSetup,
    /// The ledger's files.
    pub(crate) files: &'p Files<'g>,
    /// The ledger's lease refs.
    pub(crate) refs: &'p Refs<'g>,
}

/// The ledger's transport: `gh` with the ledger token, and the object-only repository in a
/// scratch directory that ref deletions are pushed from.
pub(crate) struct LedgerTransport {
    gh: GhSetup,
    ledger_gh: Gh,
    ledger: Repository,
    pusher: ObjectRepo,
    remote: PushRemote,
    scratch: TempDir,
}

/// What [`LedgerTransport::open`] reads from the job's environment.
pub(crate) struct Programs {
    /// `PATH`, which finds `gh` (and git, through forge's environment).
    pub(crate) path: Option<OsString>,
    /// What `gh` inherits.
    pub(crate) gh: GhEnvironment,
}

impl LedgerTransport {
    /// The ledger `ledger`'s transport for `job`, authenticated as `token`, with `gh` and git
    /// found on `programs.path`.
    ///
    /// In order: `gh` on `PATH`, the killer, the push remote, the scratch directory, the
    /// object-only repository in it and the `gh` transport. The scratch directory is removed
    /// again when a later step fails, and pushed onto `left` when it cannot be.
    pub(crate) fn open(
        job: LedgerJob,
        (ledger, token): (Repository, GhToken),
        programs: Programs,
        left: &mut Vec<Left>,
    ) -> Result<Self, LedgerSetupError> {
        let no_gh = || job.refused(Cause::NoGh);
        let program = on_path("gh", programs.path.as_ref()).ok_or_else(no_gh)?;
        let path = programs.path.ok_or_else(no_gh)?;
        let killer = GroupKiller::system().map_err(|source| job.refused(Cause::Killer(source)))?;
        let remote = ledger_push_remote(&ledger, &token)
            .map_err(|source| job.refused(Cause::Remote(source)))?;
        let scratch = tempfile::tempdir().map_err(|source| job.refused(Cause::Scratch(source)))?;
        // The repository and the supervisor each keep a handle on the killer.
        let pusher = ObjectRepo::init(
            scratch.path().join("ledger.git"),
            GitEnvironment::new(path, killer.clone()),
            PUSH_LIMITS,
        )
        .map_err(|source| job.refused(Cause::Git(source)));
        let gh = GhSetup {
            job,
            supervisor: ForgeSupervisor::new(killer),
            program,
            environment: programs.gh,
        };
        let built = pusher.and_then(|pusher| Ok((pusher, gh.transport(token)?)));
        match built {
            Ok((pusher, ledger_gh)) => Ok(Self {
                gh,
                ledger_gh,
                ledger,
                pusher,
                remote,
                scratch,
            }),
            Err(error) => {
                close_scratch(job, scratch, left);
                Err(error)
            }
        }
    }

    /// Runs `with` over the ledger's ports, then closes the ledger's `gh` transport and
    /// removes the scratch directory, pushing onto `left` each directory that could not be
    /// removed.
    pub(crate) fn run<T>(
        self,
        left: &mut Vec<Left>,
        with: impl FnOnce(LedgerPorts<'_, '_>, &mut Vec<Left>) -> T,
    ) -> T {
        let Self {
            gh,
            ledger_gh,
            ledger,
            pusher,
            remote,
            scratch,
        } = self;
        let ran = {
            // The file and ref adapters each store the ledger repository.
            let files = LedgerRepo::new(Api::new(&ledger_gh, ThreadSleep), ledger.clone());
            let refs =
                LedgerRefsRepo::new(Api::new(&ledger_gh, ThreadSleep), ledger, pusher, remote);
            let ports = LedgerPorts {
                gh: &gh,
                files: &files,
                refs: &refs,
            };
            with(ports, left)
        };
        if let Err(error) = ledger_gh.close() {
            left.push(Left::Gh(LEDGER_TOKEN, error));
        }
        close_scratch(gh.job, scratch, left);
        ran
    }
}

/// Removes `job`'s scratch directory, pushing it onto `left` when it cannot be.
fn close_scratch(job: LedgerJob, scratch: TempDir, left: &mut Vec<Left>) {
    if let Err(error) = scratch.close() {
        left.push(Left::Scratch(job.name, error));
    }
}

#[cfg(test)]
mod tests;
