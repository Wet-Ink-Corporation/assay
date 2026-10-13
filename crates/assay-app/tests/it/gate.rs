//! The gate contract: exit code plus document, fail-closed (`docs/architecture/03-contracts.md`
//! §2), and the rule that required gates always run.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use assay_app::{
    BuildJob, Conclusion, ContractValidator, GateDocument, GateError, GateJob, GateOutcome,
    GateReport, GateRun, GateRunner, Plugin, PluginsConfig, Termination, account_for_required,
    run_gates,
};

const GIT: &str = "0123456789abcdef0123456789abcdef01234567";
const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";
const LINT: GateJob = GateJob::Build(BuildJob::Lint);

/// A required temper (static, lint and, through its step, test), an optional weigh-in
/// (static) and an optional exec gate (lint).
fn config() -> Result<PluginsConfig, Box<dyn Error>> {
    Ok(format!(
        r#"
[assay]
digest = "sha256:{HEX64}"

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "git:{GIT}"
required = true

[[plugin.step]]
name = "tests"
job = "test"
cmd = ["cargo", "test"]
timeout_s = 60

[[plugin]]
name = "weigh-in"
adapter = "builtin:weigh-in"
pin = "git:{GIT}"
required = false

[[plugin]]
name = "custom-lint"
adapter = "exec"
exec = ["./lint.sh"]
job = "lint"
required = false
"#
    )
    .parse()?)
}

/// A gate document from `name` with `conclusion`.
fn document(name: &str, conclusion: &str) -> Vec<u8> {
    format!(
        r#"{{"schema":"assay.findings/1","source":{{"kind":"gate","name":"{name}","version":"git:{GIT}","run_key":"k"}},"conclusion":"{conclusion}","findings":[]}}"#
    )
    .into_bytes()
}

#[derive(Debug)]
struct FakeFailure;

impl fmt::Display for FakeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the fake runner was told to fail")
    }
}

impl Error for FakeFailure {}

/// Answers each plugin with a scripted run; a plugin without a script fails to run.
#[derive(Debug, Default)]
struct FakeRunner {
    runs: BTreeMap<String, GateRun>,
    calls: Vec<(String, GateJob)>,
}

impl FakeRunner {
    fn with(name: &str, termination: Termination, document: GateDocument) -> Self {
        let mut runner = Self::default();
        runner.script(name, termination, document);
        runner
    }

    fn script(&mut self, name: &str, termination: Termination, document: GateDocument) {
        self.runs.insert(
            name.to_owned(),
            GateRun {
                termination,
                document,
            },
        );
    }
}

impl GateRunner for FakeRunner {
    type Error = FakeFailure;

    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, FakeFailure> {
        self.calls.push((plugin.name().as_str().to_owned(), job));
        self.runs.remove(plugin.name().as_str()).ok_or(FakeFailure)
    }
}

/// The outcome of `custom-lint` in `gates-lint` after one scripted run.
fn exec_outcome(
    termination: Termination,
    document: GateDocument,
) -> Result<GateOutcome, Box<dyn Error>> {
    let config = config()?;
    let validator = ContractValidator::new()?;
    let mut runner = FakeRunner::with("custom-lint", termination, document);

    let reports = run_gates(&config, LINT, &mut runner, &validator);

    reports
        .into_iter()
        .find(|report| report.plugin.as_str() == "custom-lint")
        .map(|report| report.outcome)
        .ok_or_else(|| "no report for custom-lint".into())
}

#[test]
fn exit_zero_with_a_pass_document_passes() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Exited(0),
        GateDocument::Written(document("custom-lint", "pass")),
    )?;

    assert_eq!(outcome, GateOutcome::Concluded(Conclusion::Pass));
    Ok(())
}

#[test]
fn exit_zero_takes_the_documents_failing_conclusion() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Exited(0),
        GateDocument::Written(document("custom-lint", "fail")),
    )?;

    assert_eq!(outcome, GateOutcome::Concluded(Conclusion::Fail));
    Ok(())
}

#[test]
fn exit_zero_takes_the_documents_neutral_conclusion() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Exited(0),
        GateDocument::Written(document("custom-lint", "neutral")),
    )?;

    assert_eq!(outcome, GateOutcome::Concluded(Conclusion::Neutral));
    Ok(())
}

#[test]
fn exit_one_fails_even_when_the_document_says_pass() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Exited(1),
        GateDocument::Written(document("custom-lint", "pass")),
    )?;

    assert_eq!(outcome, GateOutcome::Concluded(Conclusion::Fail));
    Ok(())
}

#[test]
fn exit_two_is_a_gate_error_whatever_the_document_says() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Exited(2),
        GateDocument::Written(document("custom-lint", "pass")),
    )?;

    assert_eq!(outcome, GateOutcome::Error(GateError::Exit(2)));
    Ok(())
}

#[test]
fn a_high_exit_code_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(Termination::Exited(127), GateDocument::Missing)?;

    assert_eq!(outcome, GateOutcome::Error(GateError::Exit(127)));
    Ok(())
}

#[test]
fn a_signal_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Signalled,
        GateDocument::Written(document("custom-lint", "pass")),
    )?;

    assert_eq!(outcome, GateOutcome::Error(GateError::Signal));
    Ok(())
}

#[test]
fn a_timeout_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::TimedOut,
        GateDocument::Written(document("custom-lint", "pass")),
    )?;

    assert_eq!(outcome, GateOutcome::Error(GateError::Timeout));
    Ok(())
}

#[test]
fn output_over_the_cap_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::OutputTooLarge,
        GateDocument::Written(document("custom-lint", "pass")),
    )?;

    assert_eq!(outcome, GateOutcome::Error(GateError::OutputTooLarge));
    Ok(())
}

#[test]
fn exit_zero_without_a_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(Termination::Exited(0), GateDocument::Missing)?;

    assert_eq!(outcome, GateOutcome::Error(GateError::MissingDocument));
    Ok(())
}

#[test]
fn exit_one_without_a_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(Termination::Exited(1), GateDocument::Missing)?;

    assert_eq!(outcome, GateOutcome::Error(GateError::MissingDocument));
    Ok(())
}

#[test]
fn an_oversized_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(Termination::Exited(0), GateDocument::TooLarge)?;

    assert_eq!(outcome, GateOutcome::Error(GateError::DocumentTooLarge));
    Ok(())
}

#[test]
fn a_document_that_is_not_a_regular_file_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(Termination::Exited(0), GateDocument::NotAFile)?;

    assert_eq!(outcome, GateOutcome::Error(GateError::InvalidDocument));
    Ok(())
}

#[test]
fn a_document_that_breaks_the_schema_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Exited(0),
        GateDocument::Written(br#"{"schema":"assay.findings/1","conclusion":"pass"}"#.to_vec()),
    )?;

    assert_eq!(outcome, GateOutcome::Error(GateError::InvalidDocument));
    Ok(())
}

#[test]
fn a_document_from_another_gate_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let outcome = exec_outcome(
        Termination::Exited(0),
        GateDocument::Written(document("temper", "pass")),
    )?;

    assert_eq!(outcome, GateOutcome::Error(GateError::InvalidDocument));
    Ok(())
}

#[test]
fn a_reviewer_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let reviewer = br#"{"schema":"assay.findings/1","source":{"kind":"reviewer","name":"custom-lint","role":"bugs","version":"v","run_key":"k"},"findings":[]}"#;

    let outcome = exec_outcome(
        Termination::Exited(0),
        GateDocument::Written(reviewer.to_vec()),
    )?;

    assert_eq!(outcome, GateOutcome::Error(GateError::InvalidDocument));
    Ok(())
}

#[test]
fn a_runner_that_cannot_run_the_gate_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let config = config()?;
    let validator = ContractValidator::new()?;
    let mut runner = FakeRunner::default();

    let reports = run_gates(&config, LINT, &mut runner, &validator);

    let outcomes: Vec<(&str, &GateOutcome)> = reports
        .iter()
        .map(|report| (report.plugin.as_str(), &report.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("temper", &GateOutcome::Error(GateError::NotRun)),
            ("custom-lint", &GateOutcome::Error(GateError::NotRun)),
        ]
    );
    Ok(())
}

#[test]
fn a_concluded_gate_keeps_its_validated_document() -> Result<(), Box<dyn Error>> {
    let config = config()?;
    let validator = ContractValidator::new()?;
    let mut runner = FakeRunner::with(
        "custom-lint",
        Termination::Exited(1),
        GateDocument::Written(document("custom-lint", "fail")),
    );

    let reports = run_gates(&config, LINT, &mut runner, &validator);

    let report = reports
        .iter()
        .find(|report| report.plugin.as_str() == "custom-lint")
        .ok_or("no report for custom-lint")?;
    let conclusion = report
        .document
        .as_ref()
        .and_then(|document| document.get("conclusion"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(conclusion, Some("fail"));
    Ok(())
}

#[test]
fn every_gate_scheduled_for_the_job_is_run_in_file_order() -> Result<(), Box<dyn Error>> {
    let config = config()?;
    let validator = ContractValidator::new()?;
    let jobs = [
        GateJob::Static,
        GateJob::Build(BuildJob::Lint),
        GateJob::Build(BuildJob::Test),
    ];

    let mut runner = FakeRunner::default();
    for job in jobs {
        let reports = run_gates(&config, job, &mut runner, &validator);
        assert!(
            reports.iter().all(|report| report.job == job),
            "every report names its job"
        );
    }

    let calls: Vec<(&str, GateJob)> = runner
        .calls
        .iter()
        .map(|(name, job)| (name.as_str(), *job))
        .collect();
    assert_eq!(
        calls,
        [
            ("temper", GateJob::Static),
            ("weigh-in", GateJob::Static),
            ("temper", GateJob::Build(BuildJob::Lint)),
            ("custom-lint", GateJob::Build(BuildJob::Lint)),
            ("temper", GateJob::Build(BuildJob::Test)),
        ]
    );
    Ok(())
}

#[test]
fn a_required_gate_without_a_report_is_a_skipped_gate_error() -> Result<(), Box<dyn Error>> {
    let config = config()?;
    let mut reports: Vec<GateReport> = Vec::new();

    account_for_required(&config, LINT, &mut reports);

    let outcomes: Vec<(&str, bool, &GateOutcome)> = reports
        .iter()
        .map(|report| (report.plugin.as_str(), report.required, &report.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [("temper", true, &GateOutcome::Error(GateError::Skipped))]
    );
    Ok(())
}

#[test]
fn a_required_gate_with_a_report_is_left_alone() -> Result<(), Box<dyn Error>> {
    let config = config()?;
    let validator = ContractValidator::new()?;
    let mut runner = FakeRunner::with(
        "temper",
        Termination::Exited(0),
        GateDocument::Written(document("temper", "pass")),
    );
    let mut reports = run_gates(&config, GateJob::Static, &mut runner, &validator);
    let before = reports.clone();

    account_for_required(&config, GateJob::Static, &mut reports);

    assert_eq!(reports, before);
    Ok(())
}

#[test]
fn gate_errors_display_as_their_reason_code() {
    let shown: Vec<String> = [
        GateError::Exit(3),
        GateError::Signal,
        GateError::Timeout,
        GateError::OutputTooLarge,
        GateError::MissingDocument,
        GateError::DocumentTooLarge,
        GateError::InvalidDocument,
        GateError::NotRun,
        GateError::Skipped,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();

    assert_eq!(
        shown,
        [
            "gate-error:exit-3",
            "gate-error:signal",
            "gate-error:timeout",
            "gate-error:output-too-large",
            "gate-error:missing-document",
            "gate-error:document-too-large",
            "gate-error:invalid-document",
            "gate-error:not-run",
            "gate-error:skipped",
        ]
    );
}

/// Fails every run with an error it classifies as a degraded ratchet.
#[derive(Debug, Default)]
struct DegradedRunner;

impl GateRunner for DegradedRunner {
    type Error = FakeFailure;

    fn run(&mut self, _plugin: &Plugin, _job: GateJob) -> Result<GateRun, FakeFailure> {
        Err(FakeFailure)
    }

    fn gate_error(_error: &FakeFailure) -> GateError {
        GateError::RatchetDegraded
    }
}

#[test]
fn a_runner_error_is_recorded_as_the_gate_error_its_runner_names() -> Result<(), Box<dyn Error>> {
    let config = config()?;
    let validator = ContractValidator::new()?;

    let reports = run_gates(&config, GateJob::Static, &mut DegradedRunner, &validator);

    let outcomes: Vec<(&str, GateOutcome)> = reports
        .iter()
        .map(|report| (report.plugin.as_str(), report.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("temper", GateOutcome::Error(GateError::RatchetDegraded)),
            ("weigh-in", GateOutcome::Error(GateError::RatchetDegraded)),
        ]
    );
    Ok(())
}

#[test]
fn a_degraded_ratchet_displays_as_its_reason_code() {
    assert_eq!(
        GateError::RatchetDegraded.to_string(),
        "gate-error:ratchet-degraded"
    );
}
