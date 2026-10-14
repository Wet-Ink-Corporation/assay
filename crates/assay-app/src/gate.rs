//! The gate contract (`docs/architecture/03-contracts.md` §2): a gate runs as a process, writes
//! one `assay.findings/1` document and exits. Exit 0 or 1 counts only together with a valid
//! document from that gate; anything else is a fail-closed `gate-error:<detail>`.
//!
//! [`run_gates`] hands every plugin scheduled for a job to the [`GateRunner`] port, so a
//! required gate cannot be left out, and [`account_for_required`] turns a required gate that
//! reported nothing into `gate-error:skipped`.

mod job;

use std::borrow::Cow;
use std::convert::Infallible;

use assay_domain::GateConclusion;
use serde_json::Value;
use thiserror::Error;

pub use job::{BuildJob, GateJob};

use crate::config::{Adapter, GateName, Plugin, PluginsConfig};
use crate::contract::{Contract, ContractValidator};

/// The port through which the `gate` use case runs one plugin's work in one job. Implemented by
/// the adapters in `assay-gates`.
pub trait GateRunner {
    /// Why the runner could not run the gate at all (it could not start, or it is not allowed
    /// in this job). Reported as `gate-error:not-run`.
    type Error: std::error::Error;

    /// Runs `plugin`'s work for `job` and reports how its process ended and what it wrote.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when the gate could not be run; the use case records that as
    /// [`Self::gate_error`] of it, never as a pass.
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error>;

    /// The gate error a failed [`run`](Self::run) is recorded as: [`GateError::NotRun`] unless
    /// the runner names a more precise one (the temper adapter's degraded ratchet is
    /// [`GateError::RatchetDegraded`]).
    fn gate_error(_error: &Self::Error) -> GateError {
        GateError::NotRun
    }
}

/// The raw facts of one gate run, before assay believes any of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRun {
    /// How the gate's process ended.
    pub termination: Termination,
    /// The document it left in its output directory.
    pub document: GateDocument,
}

/// How a gate's process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// It exited by itself with this code.
    Exited(i32),
    /// A signal ended it.
    Signalled,
    /// It was still running at its deadline and was killed.
    TimedOut,
    /// It wrote more than the output cap and was killed.
    OutputTooLarge,
}

/// The document a gate left behind, read under a size cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDocument {
    /// No document was written.
    Missing,
    /// The document is larger than the cap and was not read.
    TooLarge,
    /// The document path is not a regular file (a symlink, a directory, a FIFO).
    NotAFile,
    /// The document's bytes, not yet validated.
    Written(Vec<u8>),
}

/// What assay concludes from one gate run in one job. The domain's
/// [`GateOutcome`](assay_domain::GateOutcome) is what `decide` reads of the gate across jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateJobOutcome {
    /// The gate ran cleanly: exit 0 with the document's conclusion, or exit 1 (fail). The
    /// gate's own verdict is believed only from such a run.
    Concluded(GateConclusion),
    /// Anything else: fail-closed, reason `gate-error:<detail>`.
    Error(GateError),
}

/// Why a gate run counts as a gate error. Displayed as the reason detail, `gate-error:<detail>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GateError {
    /// The process exited with a code other than 0 or 1.
    #[error("gate-error:exit-{0}")]
    Exit(i32),
    /// A signal ended the process.
    #[error("gate-error:signal")]
    Signal,
    /// The process outlived its timeout.
    #[error("gate-error:timeout")]
    Timeout,
    /// The process wrote more output than allowed.
    #[error("gate-error:output-too-large")]
    OutputTooLarge,
    /// The process exited 0 or 1 without writing a document.
    #[error("gate-error:missing-document")]
    MissingDocument,
    /// The document is larger than the cap.
    #[error("gate-error:document-too-large")]
    DocumentTooLarge,
    /// The document is not a regular file, breaks the `assay.findings/1` schema, or is not a
    /// gate document from this gate.
    #[error("gate-error:invalid-document")]
    InvalidDocument,
    /// The runner could not run the gate.
    #[error("gate-error:not-run")]
    NotRun,
    /// A required gate reported nothing.
    #[error("gate-error:skipped")]
    Skipped,
    /// The temper ratchet did not diff against the expected merge base (03 §2.1 step 5): its
    /// mode is not `diff+totals`, a note says only the totals were compared, or its base is
    /// not the merge base assay computed.
    #[error("gate-error:ratchet-degraded")]
    RatchetDegraded,
}

impl GateError {
    /// The reason detail, `<detail>` of `gate-error:<detail>`.
    pub(crate) fn detail(self) -> Cow<'static, str> {
        match self {
            Self::Exit(code) => Cow::Owned(format!("exit-{code}")),
            Self::Signal => Cow::Borrowed("signal"),
            Self::Timeout => Cow::Borrowed("timeout"),
            Self::OutputTooLarge => Cow::Borrowed("output-too-large"),
            Self::MissingDocument => Cow::Borrowed("missing-document"),
            Self::DocumentTooLarge => Cow::Borrowed("document-too-large"),
            Self::InvalidDocument => Cow::Borrowed("invalid-document"),
            Self::NotRun => Cow::Borrowed("not-run"),
            Self::Skipped => Cow::Borrowed("skipped"),
            Self::RatchetDegraded => Cow::Borrowed("ratchet-degraded"),
        }
    }
}

/// One gate's result in one job, as that job reports it. `decide` reads the reports of every
/// job into the domain's [`GateReport`](assay_domain::GateReport).
#[derive(Debug, Clone, PartialEq)]
pub struct GateJobReport {
    /// The plugin.
    pub plugin: GateName,
    /// The job it ran in.
    pub job: GateJob,
    /// Whether the plugin is required.
    pub required: bool,
    /// What assay concludes.
    pub outcome: GateJobOutcome,
    /// The validated document, present exactly when the outcome is [`GateJobOutcome::Concluded`].
    pub document: Option<Value>,
    /// Why the outcome is an error, in assay's own words (a runner or validation error), for
    /// logs. A runner error is followed by each of its sources, joined by `: `. Never the gate's
    /// output.
    pub cause: Option<String>,
}

/// Runs every plugin scheduled for `job`, in file order, and reports each one.
///
/// A plugin is scheduled for the jobs its adapter works in: temper in `gates-static`,
/// `gates-lint` and the job of each declared step; weigh-in in `gates-static`; an `exec` gate
/// in its own Z2b job. There is no filter: required and optional gates alike are handed to
/// `runner`.
pub fn run_gates<R: GateRunner>(
    config: &PluginsConfig,
    job: GateJob,
    runner: &mut R,
    validator: &ContractValidator,
) -> Vec<GateJobReport> {
    let Ok(reports) = run_observed(config, runner, validator, &mut Unobserved(job));
    reports
}

/// What sees each gate of one job start and end: local mode streams them as events (#130).
pub(crate) trait GateObserver {
    /// Why the observer could not take a gate; the job stops at once.
    type Error;

    /// The job whose gates it sees.
    fn job(&self) -> GateJob;

    /// `plugin` is about to run in the job.
    fn started(&mut self, plugin: &Plugin) -> Result<(), Self::Error>;

    /// The gate of `report` ended; `exit` is its process's exit code, when it exited by itself.
    fn ended(&mut self, report: &GateJobReport, exit: Option<i32>) -> Result<(), Self::Error>;
}

/// No observer of the job's gates.
struct Unobserved(GateJob);

impl GateObserver for Unobserved {
    type Error = Infallible;

    fn job(&self) -> GateJob {
        self.0
    }

    fn started(&mut self, _plugin: &Plugin) -> Result<(), Self::Error> {
        Ok(())
    }

    fn ended(&mut self, _report: &GateJobReport, _exit: Option<i32>) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// [`run_gates`] for the observer's job, telling `observer` as each gate starts and ends. A
/// failing observer stops the job before the next gate starts.
pub(crate) fn run_observed<R: GateRunner, O: GateObserver>(
    config: &PluginsConfig,
    runner: &mut R,
    validator: &ContractValidator,
    observer: &mut O,
) -> Result<Vec<GateJobReport>, O::Error> {
    let job = observer.job();
    let mut reports = Vec::new();
    for plugin in config
        .plugins()
        .iter()
        .filter(|plugin| scheduled(plugin, job))
    {
        observer.started(plugin)?;
        let run = runner.run(plugin, job);
        let exit = match &run {
            Ok(GateRun {
                termination: Termination::Exited(code),
                ..
            }) => Some(*code),
            Ok(_) | Err(_) => None,
        };
        let classified = run
            .map_err(|error| Rejected::because(R::gate_error(&error), chain(&error)))
            .and_then(|run| classify(plugin.name(), run, validator));
        let (outcome, document, cause) = match classified {
            Ok((conclusion, document)) => {
                (GateJobOutcome::Concluded(conclusion), Some(document), None)
            }
            Err(rejected) => (GateJobOutcome::Error(rejected.error), None, rejected.cause),
        };
        let report = GateJobReport {
            plugin: plugin.name().clone(),
            job,
            required: plugin.required(),
            outcome,
            document,
            cause,
        };
        observer.ended(&report, exit)?;
        reports.push(report);
    }
    Ok(reports)
}

/// Appends `gate-error:skipped` for every required plugin scheduled for `job` that has no report
/// for that job in `reports`. A missing required gate is never a pass.
pub fn account_for_required(
    config: &PluginsConfig,
    job: GateJob,
    reports: &mut Vec<GateJobReport>,
) {
    let skipped: Vec<GateJobReport> = config
        .plugins()
        .iter()
        .filter(|plugin| plugin.required() && scheduled(plugin, job))
        .filter(|plugin| {
            !reports
                .iter()
                .any(|report| report.job == job && report.plugin == *plugin.name())
        })
        .map(|plugin| GateJobReport {
            plugin: plugin.name().clone(),
            job,
            required: true,
            outcome: GateJobOutcome::Error(GateError::Skipped),
            document: None,
            cause: None,
        })
        .collect();
    reports.extend(skipped);
}

/// Whether `plugin` has work in `job`.
pub(crate) fn scheduled(plugin: &Plugin, job: GateJob) -> bool {
    match plugin.adapter() {
        Adapter::Temper { steps, .. } => match job {
            GateJob::Static | GateJob::Build(BuildJob::Lint) => true,
            GateJob::Build(build) => steps.iter().any(|step| step.job() == build),
        },
        Adapter::WeighIn { .. } => job == GateJob::Static,
        Adapter::Exec { job: own, .. } => job == GateJob::Build(*own),
    }
}

/// `error` and every `source()` below it, joined by `: `, so a runner error keeps the I/O error
/// that caused it. The chain is assay's own error types, never the gate's output.
fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    for cause in std::iter::successors(error.source(), |cause| cause.source()) {
        text.push_str(": ");
        text.push_str(&cause.to_string());
    }
    text
}

/// A run that does not count, and why in assay's own words.
struct Rejected {
    error: GateError,
    cause: Option<String>,
}

impl Rejected {
    /// `error` with an explanation for the logs.
    const fn because(error: GateError, cause: String) -> Self {
        Self {
            error,
            cause: Some(cause),
        }
    }
}

impl From<GateError> for Rejected {
    fn from(error: GateError) -> Self {
        Self { error, cause: None }
    }
}

/// The conclusion and the validated document of one run of `gate`, or why it does not count.
fn classify(
    gate: &GateName,
    run: GateRun,
    validator: &ContractValidator,
) -> Result<(GateConclusion, Value), Rejected> {
    let exit_failed = match run.termination {
        Termination::Exited(0) => false,
        Termination::Exited(1) => true,
        Termination::Exited(code) => return Err(GateError::Exit(code).into()),
        Termination::Signalled => return Err(GateError::Signal.into()),
        Termination::TimedOut => return Err(GateError::Timeout.into()),
        Termination::OutputTooLarge => return Err(GateError::OutputTooLarge.into()),
    };
    let bytes = match run.document {
        GateDocument::Written(bytes) => bytes,
        GateDocument::Missing => return Err(GateError::MissingDocument.into()),
        GateDocument::TooLarge => return Err(GateError::DocumentTooLarge.into()),
        GateDocument::NotAFile => {
            let cause = "the document path is not a regular file".to_owned();
            return Err(Rejected::because(GateError::InvalidDocument, cause));
        }
    };
    let document = validator
        .read(Contract::Findings, &bytes)
        .map_err(|error| Rejected::because(GateError::InvalidDocument, error.to_string()))?;
    let Some(conclusion) = gate_conclusion(gate, &document) else {
        let cause = format!("not a gate document from `{gate}`");
        return Err(Rejected::because(GateError::InvalidDocument, cause));
    };
    let conclusion = if exit_failed {
        GateConclusion::Fail
    } else {
        conclusion
    };
    Ok((conclusion, document))
}

/// The conclusion of a schema-valid findings document, when it is a gate document from `gate`.
fn gate_conclusion(gate: &GateName, document: &Value) -> Option<GateConclusion> {
    let source = document.get("source")?;
    if source.get("kind")?.as_str()? != "gate" || source.get("name")?.as_str()? != gate.as_str() {
        return None;
    }
    match document.get("conclusion")?.as_str()? {
        "pass" => Some(GateConclusion::Pass),
        "fail" => Some(GateConclusion::Fail),
        "neutral" => Some(GateConclusion::Neutral),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::GateError;

    #[test]
    fn a_gate_errors_detail_is_what_follows_gate_error_in_its_reason() {
        let errors = [
            GateError::Exit(3),
            GateError::Signal,
            GateError::Timeout,
            GateError::OutputTooLarge,
            GateError::MissingDocument,
            GateError::DocumentTooLarge,
            GateError::InvalidDocument,
            GateError::NotRun,
            GateError::Skipped,
            GateError::RatchetDegraded,
        ];

        for error in errors {
            assert_eq!(
                format!("gate-error:{}", error.detail()),
                error.to_string(),
                "{error:?}"
            );
        }
    }
}
