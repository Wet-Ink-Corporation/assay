//! `assay gate <job> --config <plugins-toml> --head <dir> --out <dir>` (#67): the entry point a
//! Z2 gate job runs (02 §3, 03 §2, ADR-12).
//!
//! In `gates-test` it runs every plugin base config schedules there (the temper plugin's
//! declared `test` steps, and each `exec` gate whose `job` is `test`) through
//! [`assay_app::run_gates`], from the head tree with the gate environment, and writes each
//! concluded gate's validated `assay.findings/1` document as `<out>/<plugin>.json`
//! (`gate/test_job.rs`). In `gates-static`, which also takes `--node <abs> --temper <abs>`
//! (#60 ST-7), it fetches the pinned base and head into an object-only repository
//! (`gate/fetch.rs`) and runs the temper Z2s checks and Weigh-In the same way
//! (`gate/static_job.rs`). In `gates-lint`, which takes the same programs and `ASSAY_MERGE_BASE`
//! as its ratchet base (#60 ST-8), it fetches them the same way, refuses a `--head` checkout
//! that is not the head commit's tree (`gate/checkout.rs`, #348), and runs temper's declared
//! `lint` steps, the pinned `baseline --check` and the `exec` gates of `job = "lint"` there
//! (`gate/lint_job.rs`).
//!
//! The base config is read from outside the head tree (05 T3). `--out` must not exist and its
//! parent must be named by its resolved path (no symlink, no `..`): it is checked before
//! anything runs and created only after the gates ran (one that appeared meanwhile is
//! refused), so nothing a test wrote before the gates finished is in it; a process that
//! escaped its group with `setsid` is 05 T6's accepted residual. Each document goes in through
//! a dot-named temporary file that is persisted without replacing anything. The output is
//! bounded: one document per plugin of a config of at most 1 MiB, each re-encoded compact from
//! what the validator read under [`assay_app::MAX_DOCUMENT_BYTES`], so no larger. Loops are
//! over base config's plugins and their reports only: O(plugins) writes.
//!
//! The job's `gates` output (#60 ST-6, 03 §4a) is appended to the regular file `GITHUB_OUTPUT`
//! names, opened (never created) before anything runs: one entry per gate in file order, with
//! the sha256 of the exact bytes of each concluded gate's document. The documents and the
//! output are all encoded before anything is written, the documents are written next and the
//! output last, so a job that cannot encode or write its documents appends nothing.

mod checkout;
mod error;
mod fetch;
mod lint_job;
mod paths;
mod results;
mod static_job;
mod test_job;

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use assay_app::{
    BuildJob, ContractValidator, GateJob, GateJobOutcome, GateJobReport, PluginsConfig,
    account_for_required,
};
use assay_domain::GateConclusion;
use assay_forge::process::GroupKiller;
use tempfile::TempDir;

pub(crate) use self::error::{GateCliError, Variable};
use self::error::{PinnedError, Program, Side};
pub(crate) use self::fetch::PinnedVariables;
use self::lint_job::LINT;
use self::paths::{Placed, Programs, absolute, refuse_paths};
use crate::doctor;
use crate::guard_job::open_output;

/// `gates-test`.
const TEST: GateJob = GateJob::Build(BuildJob::Test);

/// What the composition root read of the process for the job.
#[derive(Debug)]
pub(crate) struct GateVariables {
    /// `ASSAY_RUN_KEY`: the run the documents belong to.
    pub(crate) run_key: Option<OsString>,
    /// `GITHUB_OUTPUT`: the file the job's outputs are appended to.
    pub(crate) output: Option<OsString>,
    /// What `gates-static` and `gates-lint` fetch with, and `gates-lint`'s merge base;
    /// `gates-test` reads none of it.
    pub(crate) fetch: PinnedVariables,
    /// Every variable; only the gate allowlist reaches a gate.
    pub(crate) vars: Vec<(OsString, OsString)>,
}

/// A job that ran and wrote its documents.
#[derive(Debug)]
pub(crate) struct Ran {
    reports: Vec<GateJobReport>,
    /// The job's scratch directory, when it could not be removed.
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
    node: Option<PathBuf>,
    temper: Option<PathBuf>,
}

/// `<job> --config <path> --head <path> --out <path> [--node <path>] [--temper <path>]`, the
/// flags in any order, each once; which job takes `--node` and `--temper` is [`wire`]'s.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, GateCliError> {
    let job = args.next().ok_or(GateCliError::Usage)?;
    let job = [GateJob::Static, LINT, TEST]
        .into_iter()
        .find(|known| job.to_str() == Some(known.to_string().as_str()))
        .ok_or(GateCliError::Usage)?;
    let (mut config, mut head, mut out, mut node, mut temper) = (None, None, None, None, None);
    while let Some(flag) = args.next() {
        let slot = match flag.to_str() {
            Some("--config") => &mut config,
            Some("--head") => &mut head,
            Some("--out") => &mut out,
            Some(flag) if flag == Program::Node.flag() => &mut node,
            Some(flag) if flag == Program::Temper.flag() => &mut temper,
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
            node,
            temper,
        }),
        _ => Err(GateCliError::Usage),
    }
}

/// A job `assay gate` runs, with what only it takes.
#[derive(Debug)]
enum Wired {
    Test,
    Static(Programs),
    Lint(Programs),
}

/// The job, once its flags fit it: `gates-static` and `gates-lint` take both programs,
/// absolute, and `gates-test` neither.
fn wire(
    job: GateJob,
    node: Option<PathBuf>,
    temper: Option<PathBuf>,
) -> Result<Wired, GateCliError> {
    match (job, node, temper) {
        (GateJob::Build(BuildJob::Test), None, None) => Ok(Wired::Test),
        (GateJob::Static, Some(node), Some(temper)) => {
            Ok(Wired::Static(Programs::absolute(node, temper)?))
        }
        (GateJob::Build(BuildJob::Lint), Some(node), Some(temper)) => {
            Ok(Wired::Lint(Programs::absolute(node, temper)?))
        }
        // gates-static or gates-lint without both programs, gates-test with either.
        (GateJob::Static | GateJob::Build(BuildJob::Lint), None, _)
        | (GateJob::Static | GateJob::Build(BuildJob::Lint), _, None)
        | (GateJob::Build(BuildJob::Test), Some(_), _)
        | (GateJob::Build(BuildJob::Test), _, Some(_)) => Err(GateCliError::Usage),
    }
}

/// The job's paths as given.
struct Paths {
    config: PathBuf,
    head: PathBuf,
    out: PathBuf,
}

/// What every job takes of the variables.
struct JobInputs {
    /// The run key, non-empty.
    run_key: String,
    /// `GITHUB_OUTPUT`'s path.
    output: OsString,
    /// Every variable; only the gate allowlist reaches a gate.
    vars: Vec<(OsString, OsString)>,
}

/// Runs the job named by `args` (after `gate`), writes its documents and appends its `gates`
/// output.
///
/// # Errors
///
/// [`GateCliError`] when the arguments, the variables, `GITHUB_OUTPUT`, the paths, the
/// programs or the config are refused, before anything runs; when `gates-static` or
/// `gates-lint` cannot fetch its pinned commits; when `gates-lint`'s checkout is not the head
/// commit's tree, before any gate runs; or when the documents or the output cannot be encoded
/// or written. Nothing is then promised, and no output is appended unless every document was
/// written.
pub(crate) fn run(
    args: impl Iterator<Item = OsString>,
    variables: GateVariables,
) -> Result<Ran, GateCliError> {
    let Args {
        job,
        config,
        head,
        out,
        node,
        temper,
    } = parse(args)?;
    let wired = wire(job, node, temper)?;
    let run_key = variables
        .run_key
        .filter(|value| !value.is_empty())
        .ok_or(GateCliError::RunKey)?
        .into_string()
        .map_err(|_not_unicode| GateCliError::RunKeyNotUnicode)?;
    let output = variables.output.ok_or(GateCliError::OutputVariable)?;
    let paths = Paths { config, head, out };
    let job = JobInputs {
        run_key,
        output,
        vars: variables.vars,
    };
    match wired {
        Wired::Test => test_job::run(&paths, job),
        Wired::Static(programs) => static_job::run(&paths, programs, variables.fetch, job),
        Wired::Lint(programs) => lint_job::run(&paths, programs, variables.fetch, job),
    }
}

/// What every job reads, and refuses, before anything runs.
struct Prepared {
    placed: Placed,
    output: File,
    config: PluginsConfig,
    validator: ContractValidator,
    killer: GroupKiller,
}

/// Refuses the paths, opens `output` and reads base config, the schemas and the killer.
fn prepare(paths: &Paths, output: &OsStr) -> Result<Prepared, GateCliError> {
    let placed = refuse_paths(&paths.config, &paths.head, &absolute(&paths.out)?)?;
    let output = open_output(output).map_err(GateCliError::OutputFile)?;
    let config = doctor::read_pins(&placed.config).map_err(GateCliError::Config)?;
    let validator = ContractValidator::new().map_err(GateCliError::Validator)?;
    let killer = GroupKiller::system().map_err(GateCliError::Killer)?;
    Ok(Prepared {
        placed,
        output,
        config,
        validator,
        killer,
    })
}

/// The job's scratch directory, under the system's temporary directory.
fn scratch() -> Result<TempDir, GateCliError> {
    tempfile::tempdir().map_err(|source| GateCliError::Scratch {
        path: std::env::temp_dir(),
        source,
    })
}

/// Where a job's results go.
struct Finished<'p> {
    job: GateJob,
    config: &'p PluginsConfig,
    out: &'p Path,
    output: File,
}

/// Accounts for the required gates that did not report, then writes the documents and the
/// output; the reports are the job's.
fn finish(
    finished: Finished<'_>,
    mut reports: Vec<GateJobReport>,
) -> Result<Vec<GateJobReport>, GateCliError> {
    account_for_required(finished.config, finished.job, &mut reports);
    results::write(finished.out, &reports, finished.output)?;
    Ok(reports)
}

/// Removes the scratch directory once the job ended with `ended`, on every path: a directory
/// that could not be removed is reported beside the job's error, or as left behind by a job
/// that ran (H-10: `TempDir`'s drop would discard the failure).
fn closing(
    scratch: TempDir,
    ended: Result<Vec<GateJobReport>, GateCliError>,
) -> Result<Ran, GateCliError> {
    let left = close(scratch);
    match (ended, left) {
        (Ok(reports), scratch_left) => Ok(Ran {
            reports,
            scratch_left,
        }),
        (Err(error), None) => Err(error),
        (Err(error), Some(left)) => Err(GateCliError::ScratchLeft {
            error: Box::new(error),
            left: Box::new(left),
        }),
    }
}

/// Removes the scratch directory, saying where it is left when it cannot be removed.
fn close(scratch: TempDir) -> Option<GateCliError> {
    let path = scratch.path().to_path_buf();
    scratch
        .close()
        .err()
        .map(|source| GateCliError::Scratch { path, source })
}

#[cfg(test)]
mod tests;
