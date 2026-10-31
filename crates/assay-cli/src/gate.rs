//! `assay gate <job> --config <plugins-toml> --head <dir> --out <dir>` (#67): the entry point a
//! Z2 gate job runs (02 §3, 03 §2, ADR-12).
//!
//! In `gates-test` it runs every plugin base config schedules there (the temper plugin's
//! declared `test` steps, and each `exec` gate whose `job` is `test`) through
//! [`assay_app::run_gates`], from the head tree with the gate environment, and writes each
//! concluded gate's validated `assay.findings/1` document as `<out>/<plugin>.json`. Nothing of
//! `gates-lint` is taken: no `node`, no pinned temper, no object-only repository, no ratchet base.
//! `gates-lint` and `gates-static` are named but refused: their inputs (the object-only repository
//! with base and head, and Z2s's merge base) are not wired into any job yet.
//!
//! The base config is read from outside the head tree (05 T3). `--out` must not exist and its
//! parent must be named by its resolved path (no symlink, no `..`): it is checked before
//! anything runs and created only after the gates ran (one that appeared meanwhile is
//! refused), so nothing a test wrote before the gates finished is in it; a process that
//! escaped its group with `setsid` is 05 T6's accepted residual. Each document goes in through
//! a dot-named temporary file that is persisted without replacing anything. The output is bounded: one document per plugin of a
//! config of at most 1 MiB, each re-encoded compact from what the validator read under
//! [`assay_app::MAX_DOCUMENT_BYTES`], so no larger. Loops are over base config's plugins and
//! their reports only: O(plugins) writes.
//!
//! The job's `gates` output (#60 ST-6, 03 §4a) is appended to the regular file `GITHUB_OUTPUT`
//! names, opened (never created) before anything runs: one entry per gate in file order, with
//! the sha256 of the exact bytes of each concluded gate's document. The documents and the
//! output are all encoded before anything is written, the documents are written next and the
//! output last, so a job that cannot encode or write its documents appends nothing.

mod results;

use std::convert::Infallible;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{self, Path, PathBuf};

use assay_app::{
    Adapter, BuildJob, ContractSchemaError, ContractValidator, GateJob, GateJobOutcome,
    GateJobReport, GateRun, GateRunner, Plugin, account_for_required, run_gates,
};
use assay_cli::outputs::GatesOutputError;
use assay_cli::{ForgeSupervisor, OutputError};
use assay_domain::GateConclusion;
use assay_forge::process::{GroupKiller, KillerError, ProcessError};
use assay_gates::{
    ExecAdapter, ExecError, ExecLimits, ExecSettings, ExecSettingsError, GateEnvironment,
    TemperError, TemperSettingsError, TemperStepSettings, TemperTestAdapter,
};
use tempfile::TempDir;

use crate::doctor::{self, DoctorCliError, write_chain};
use crate::guard_job::{OUTPUT, open_output};
use crate::self_gate::RUN_KEY;

/// The only job `assay gate` runs so far.
const TEST: GateJob = GateJob::Build(BuildJob::Test);

/// Why the job could not run, or its results could not be written; exit 2.
#[derive(Debug)]
pub(crate) enum GateCliError {
    /// Not `<job> --config <path> --head <path> --out <path>`, each flag once.
    Usage,
    /// The job is named but its inputs are not wired yet.
    NotWired(GateJob),
    /// `ASSAY_RUN_KEY` is unset or empty.
    RunKey,
    /// `ASSAY_RUN_KEY` is not valid Unicode.
    RunKeyNotUnicode,
    /// A path could not be resolved or inspected.
    Path { path: PathBuf, source: io::Error },
    /// `--head` is not a directory.
    HeadNotDir(PathBuf),
    /// `--config` is inside `--head`, which the PR controls.
    ConfigInHead(PathBuf),
    /// `--config` is not a regular file (a directory, a FIFO, a device).
    NotAFile(PathBuf),
    /// `--out` already exists.
    OutExists(PathBuf),
    /// `--out` is inside `--head`, where the PR's code could write first.
    OutInHead(PathBuf),
    /// `--out`'s parent is not written resolved: a symlink or `..` is on the way.
    OutNotResolved { given: PathBuf, resolved: PathBuf },
    /// The base config could not be read or was refused.
    Config(DoctorCliError),
    /// No process-group killer is available on this host.
    Killer(KillerError),
    /// The temper adapter's settings were refused.
    TemperSettings(TemperSettingsError),
    /// The exec adapter's settings were refused.
    ExecSettings(ExecSettingsError),
    /// The contract schemas did not compile.
    Validator(ContractSchemaError),
    /// The exec gates' scratch directory could not be created or removed.
    Scratch { path: PathBuf, source: io::Error },
    /// A concluded report carries no document (the gate use case promises one).
    NoDocument(String),
    /// A document could not be encoded.
    Encode(serde_json::Error),
    /// The output directory or a document could not be written.
    Write { path: PathBuf, source: io::Error },
    /// `GITHUB_OUTPUT` is not set.
    OutputVariable,
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// The `gates` output could not be encoded; nothing was written.
    GatesOutput(GatesOutputError),
    /// The `gates` output could not be appended.
    Output(OutputError),
}

impl fmt::Display for GateCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str(
                "usage: assay gate <gates-static|gates-lint|gates-test> --config <plugins-toml> \
                 --head <dir> --out <dir>",
            ),
            Self::NotWired(job) => write!(
                f,
                "gate {job}: not wired yet (its object-only repository and ratchet base inputs \
                 are not built); only gates-test runs"
            ),
            Self::RunKey => write!(f, "{RUN_KEY} is not set"),
            Self::RunKeyNotUnicode => write!(f, "{RUN_KEY} is not valid Unicode"),
            Self::Path { path, source } => write!(f, "cannot resolve {}: {source}", path.display()),
            Self::HeadNotDir(path) => {
                write!(f, "the head tree {} is not a directory", path.display())
            }
            Self::ConfigInHead(path) => write!(
                f,
                "the config {} is inside the head tree; base config must come from base",
                path.display()
            ),
            Self::NotAFile(path) => {
                write!(f, "the config {} is not a regular file", path.display())
            }
            Self::OutExists(path) => write!(f, "{} already exists", path.display()),
            Self::OutInHead(path) => write!(
                f,
                "the output directory {} is inside the head tree",
                path.display()
            ),
            Self::OutNotResolved { given, resolved } => write!(
                f,
                "the output directory {} passes through a symlink or `..` (its parent is {}); \
                 name it by its resolved path",
                given.display(),
                resolved.display()
            ),
            Self::Config(source) => write!(f, "{source}"),
            Self::Killer(source) => write_chain(f, source),
            Self::TemperSettings(source) => write!(f, "{source}"),
            Self::ExecSettings(source) => write!(f, "{source}"),
            Self::Validator(source) => write_chain(f, source),
            Self::Scratch { path, source } => {
                write!(f, "scratch directory {}: {source}", path.display())
            }
            Self::NoDocument(plugin) => write!(f, "gate `{plugin}` concluded without a document"),
            Self::Encode(source) => write!(f, "cannot encode a document: {source}"),
            Self::Write { path, source } => write!(f, "cannot write {}: {source}", path.display()),
            Self::OutputVariable => write!(f, "{OUTPUT} is not set"),
            Self::OutputFile(source) => write!(f, "opening {OUTPUT}: {source}"),
            Self::GatesOutput(source) => write!(f, "{source}"),
            Self::Output(source) => write_chain(f, source),
        }
    }
}

/// What the composition root read of the process for the job.
#[derive(Debug)]
pub(crate) struct GateVariables {
    /// `ASSAY_RUN_KEY`: the run the documents belong to.
    pub(crate) run_key: Option<OsString>,
    /// `GITHUB_OUTPUT`: the file the job's outputs are appended to.
    pub(crate) output: Option<OsString>,
    /// Every variable; only the gate allowlist reaches a gate.
    pub(crate) vars: Vec<(OsString, OsString)>,
}

/// A job that ran and wrote its documents.
#[derive(Debug)]
pub(crate) struct Ran {
    reports: Vec<GateJobReport>,
    /// The exec scratch directory, when it could not be removed.
    pub(crate) scratch_left: Option<GateCliError>,
}

impl Ran {
    /// One line per gate, `<plugin> <pass|fail|neutral|gate-error:<detail>>`, in file order.
    pub(crate) fn write(&self, mut out: impl Write) -> io::Result<()> {
        for report in &self.reports {
            match report.outcome {
                GateJobOutcome::Concluded(conclusion) => {
                    writeln!(out, "{} {}", report.plugin, conclusion_name(conclusion))?;
                }
                GateJobOutcome::Error(error) => writeln!(out, "{} {error}", report.plugin)?,
            }
        }
        out.flush()
    }

    /// Why each gate error happened, in assay's own words, one line each.
    pub(crate) fn write_causes(&self, mut out: impl Write) -> io::Result<()> {
        for report in &self.reports {
            if let Some(cause) = &report.cause {
                writeln!(out, "assay: {}: {cause}", report.plugin)?;
            }
        }
        out.flush()
    }
}

const fn conclusion_name(conclusion: GateConclusion) -> &'static str {
    match conclusion {
        GateConclusion::Pass => "pass",
        GateConclusion::Fail => "fail",
        GateConclusion::Neutral => "neutral",
    }
}

/// The job's arguments.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    job: GateJob,
    config: PathBuf,
    head: PathBuf,
    out: PathBuf,
}

/// `<job> --config <path> --head <path> --out <path>`, the flags in any order, each once.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, GateCliError> {
    let job = args.next().ok_or(GateCliError::Usage)?;
    let job = [GateJob::Static, GateJob::Build(BuildJob::Lint), TEST]
        .into_iter()
        .find(|known| job.to_str() == Some(known.to_string().as_str()))
        .ok_or(GateCliError::Usage)?;
    let (mut config, mut head, mut out) = (None, None, None);
    while let Some(flag) = args.next() {
        let slot = match flag.to_str() {
            Some("--config") => &mut config,
            Some("--head") => &mut head,
            Some("--out") => &mut out,
            _ => return Err(GateCliError::Usage),
        };
        let value = args.next().ok_or(GateCliError::Usage)?;
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(GateCliError::Usage);
        }
    }
    match (config, head, out) {
        (Some(config), Some(head), Some(out)) => Ok(Args {
            job,
            config,
            head,
            out,
        }),
        _ => Err(GateCliError::Usage),
    }
}

/// Runs the job named by `args` (after `gate`), writes its documents and appends its `gates`
/// output.
///
/// # Errors
///
/// [`GateCliError`] when the arguments, the variables, `GITHUB_OUTPUT`, the paths or the
/// config are refused, before anything runs, or when the documents or the output cannot be
/// encoded or written; nothing is then promised, and no output is appended unless every
/// document was written.
pub(crate) fn run(
    args: impl Iterator<Item = OsString>,
    variables: GateVariables,
) -> Result<Ran, GateCliError> {
    let args = parse(args)?;
    if args.job != TEST {
        return Err(GateCliError::NotWired(args.job));
    }
    let run_key = variables
        .run_key
        .filter(|value| !value.is_empty())
        .ok_or(GateCliError::RunKey)?
        .into_string()
        .map_err(|_not_unicode| GateCliError::RunKeyNotUnicode)?;
    let output = variables.output.ok_or(GateCliError::OutputVariable)?;
    let Placed { config, head, out } =
        refuse_paths(&args.config, &args.head, &absolute(&args.out)?)?;
    let output = open_output(output).map_err(GateCliError::OutputFile)?;
    let config = doctor::read_pins(&config).map_err(GateCliError::Config)?;
    let validator = ContractValidator::new().map_err(GateCliError::Validator)?;
    let killer = GroupKiller::system().map_err(GateCliError::Killer)?;
    let scratch = tempfile::tempdir().map_err(|source| GateCliError::Scratch {
        path: std::env::temp_dir(),
        source,
    })?;
    let environment = GateEnvironment::from_vars(variables.vars);
    let mut runner = TestGates::new(&head, environment, run_key, (killer, &scratch))?;
    let mut reports = run_gates(&config, TEST, &mut runner, &validator);
    account_for_required(&config, TEST, &mut reports);
    results::write(&out, &reports, output)?;
    Ok(Ran {
        reports,
        scratch_left: close(scratch),
    })
}

/// `path` made absolute against the working directory, without touching the file system.
fn absolute(path: &Path) -> Result<PathBuf, GateCliError> {
    path::absolute(path).map_err(|source| GateCliError::Path {
        path: path.to_path_buf(),
        source,
    })
}

/// `path` with every symlink resolved.
fn canonical(path: &Path) -> Result<PathBuf, GateCliError> {
    fs::canonicalize(path).map_err(|source| GateCliError::Path {
        path: path.to_path_buf(),
        source,
    })
}

/// Refuses, before anything runs, a head tree that is not a directory, a config that is not a
/// regular file outside it, and an `out` that exists in any form (a symlink included), whose
/// parent is not a directory, or that is inside the head tree. Containment is compared on
/// paths with every symlink resolved; `out`'s parent (it does not exist yet) must already be
/// written resolved, with no symlink and no `..`. Returns the resolved paths, the ones used: a
/// head tree's path with `..` in it would put the git ceiling (its parent) below it.
fn refuse_paths(config: &Path, head: &Path, out: &Path) -> Result<Placed, GateCliError> {
    let head = canonical(head)?;
    if !head.is_dir() {
        return Err(GateCliError::HeadNotDir(head));
    }
    let config = canonical(config)?;
    if config.starts_with(&head) {
        return Err(GateCliError::ConfigInHead(config));
    }
    if !config.is_file() {
        return Err(GateCliError::NotAFile(config));
    }
    match fs::symlink_metadata(out) {
        Ok(_) => return Err(GateCliError::OutExists(out.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(GateCliError::Path {
                path: out.to_path_buf(),
                source,
            });
        }
    }
    // An `out` ending in `..` names no new directory.
    let (Some(parent), Some(name)) = (out.parent(), out.file_name()) else {
        return Err(GateCliError::Usage);
    };
    let resolved = canonical(parent)?;
    // A symlink or `..` on the way is refused rather than followed: a test could retarget a
    // symlink between this check and the writes.
    if resolved != parent {
        return Err(GateCliError::OutNotResolved {
            given: out.to_path_buf(),
            resolved,
        });
    }
    let out = resolved.join(name);
    if out.starts_with(&head) {
        return Err(GateCliError::OutInHead(out));
    }
    Ok(Placed { config, head, out })
}

/// The job's paths once checked, every symlink resolved: the ones it uses.
struct Placed {
    config: PathBuf,
    head: PathBuf,
    out: PathBuf,
}

/// Removes the scratch directory, saying where it is left when it cannot be removed.
fn close(scratch: TempDir) -> Option<GateCliError> {
    let path = scratch.path().to_path_buf();
    scratch
        .close()
        .err()
        .map(|source| GateCliError::Scratch { path, source })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Args, GateCliError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn the_job_and_the_flags_parse_in_any_order() {
        let args = parsed(&["gates-test", "--out", "o", "--config", "c", "--head", "h"]);

        assert!(
            matches!(
                args,
                Ok(Args { job: TEST, ref config, ref head, ref out })
                    if config == Path::new("c") && head == Path::new("h") && out == Path::new("o")
            ),
            "{args:?}"
        );
    }

    #[test]
    fn every_gate_job_is_named() {
        let jobs: Vec<Option<GateJob>> = ["gates-static", "gates-lint", "gates-test"]
            .into_iter()
            .map(|job| {
                parsed(&[job, "--config", "c", "--head", "h", "--out", "o"])
                    .ok()
                    .map(|args| args.job)
            })
            .collect();

        assert_eq!(
            jobs,
            [
                Some(GateJob::Static),
                Some(GateJob::Build(BuildJob::Lint)),
                Some(TEST)
            ]
        );
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        let cases: [&[&str]; 7] = [
            &[],
            &["test", "--config", "c", "--head", "h", "--out", "o"],
            &["gates-test", "--config", "c", "--head", "h"],
            &["gates-test", "--config", "c", "--head", "h", "--out"],
            &[
                "gates-test",
                "--config",
                "c",
                "--config",
                "c",
                "--head",
                "h",
                "--out",
                "o",
            ],
            &[
                "gates-test",
                "--config",
                "c",
                "--head",
                "h",
                "--out",
                "o",
                "--base",
                "b",
            ],
            &["gates-test", "-c", "c", "--head", "h", "--out", "o"],
        ];

        for args in cases {
            assert!(matches!(parsed(args), Err(GateCliError::Usage)), "{args:?}");
        }
    }
}
