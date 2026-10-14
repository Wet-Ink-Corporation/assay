//! The temper adapter in `gates-test` (#67, `docs/architecture/03-contracts.md` §2.1 step 7): the
//! declared `test` steps (`tests`, `doctests`) in file order under their own timeouts, zero tests
//! failing the gate, and one retry of a step a signal ended (F9, `06` §4).
//!
//! cargo-nextest does not run here: a scripted supervisor plays each step by its first argument
//! (`nextest`, `test`), ending each attempt as scripted (exit 0 once the script is spent).

use std::assert_matches;
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    BuildJob, ContractValidator, GateError, GateJob, GateJobOutcome, GateRunner, PluginsConfig,
    Termination, run_gates,
};
use assay_domain::{FindingsDocument, GateConclusion};
use assay_gates::{CaptureSupervisor, CapturedRun, ProcessSpec, TemperAdapter, TemperError};

use crate::support::{GIT, SeenSpec, plugin};
use crate::temper_support::Pr;

const TEST: GateJob = GateJob::Build(BuildJob::Test);
const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// The spec's temper plugin: `fmt` in gates-lint, then `tests` and `doctests` in gates-test.
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
timeout_s = 1200

[[plugin.step]]
name = "fmt"
job = "lint"
cmd = ["cargo", "fmt", "--check"]
timeout_s = 300

[[plugin.step]]
name = "tests"
job = "test"
cmd = ["cargo", "nextest", "run", "--workspace", "--no-tests=fail"]
timeout_s = 1500

[[plugin.step]]
name = "doctests"
job = "test"
cmd = ["cargo", "test", "--doc"]
timeout_s = 600
"#
    )
    .parse()?)
}

#[derive(Debug)]
struct FakeError(String);

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for FakeError {}

/// Plays the declared steps, keyed by their first argument.
#[derive(Debug, Default)]
struct FakeSteps {
    /// How each attempt of a step ends, in order; exit 0 once spent.
    script: BTreeMap<String, VecDeque<Termination>>,
    /// A step the supervisor cannot start.
    cannot_start: Option<String>,
    seen: Vec<SeenSpec>,
}

impl FakeSteps {
    /// `step`'s attempts end as `attempts`, in order.
    fn ends(mut self, step: &str, attempts: &[Termination]) -> Self {
        self.script
            .insert(step.to_owned(), attempts.iter().copied().collect());
        self
    }

    /// The first argument of each process run, in order.
    fn ran(&self) -> Vec<&str> {
        self.seen
            .iter()
            .map(|seen| seen.args.first().map_or("", String::as_str))
            .collect()
    }
}

impl CaptureSupervisor for FakeSteps {
    type Error = FakeError;

    fn capture(&mut self, spec: &ProcessSpec<'_>) -> Result<CapturedRun, FakeError> {
        self.seen.push(SeenSpec {
            program: spec.program.to_path_buf(),
            args: spec.args.to_vec(),
            cwd: spec.cwd.to_path_buf(),
            env: spec.env.to_vec(),
            timeout: spec.timeout,
            max_output_bytes: spec.max_output_bytes,
        });
        let step = spec.args.first().map_or("", String::as_str);
        if self.cannot_start.as_deref() == Some(step) {
            return Err(FakeError(format!("cannot start {step}")));
        }
        let termination = self
            .script
            .get_mut(step)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Termination::Exited(0));
        Ok(CapturedRun {
            termination,
            stdout: Vec::new(),
        })
    }
}

/// Runs gates-test through the gate use case: the outcome and the validated document. The
/// settings carry no ratchet base, which gates-test does not need.
fn outcome(
    steps: &mut FakeSteps,
) -> Result<(GateJobOutcome, Option<FindingsDocument>), Box<dyn Error>> {
    let pr = Pr::new()?;
    let config = config()?;
    let mut adapter = TemperAdapter::new(steps, &pr.revisions, pr.settings()?);
    let reports = run_gates(&config, TEST, &mut adapter, &ContractValidator::new()?);
    let [report] = reports.as_slice() else {
        return Err(format!("expected one report, got {reports:?}").into());
    };
    let document = report
        .document
        .clone()
        .map(serde_json::from_value)
        .transpose()?;
    Ok((report.outcome, document))
}

const PASSED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Pass);
const FAILED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Fail);
const SIGNALLED: Termination = Termination::Signalled;

#[test]
fn runs_tests_then_doctests_each_under_its_own_timeout_from_the_head_tree()
-> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let config = config()?;
    let temper = plugin(&config, "temper")?;
    let mut steps = FakeSteps::default();
    let mut adapter = TemperAdapter::new(&mut steps, &pr.revisions, pr.settings()?);

    adapter.run(&temper, TEST)?;

    let seen: Vec<(PathBuf, Vec<&str>, PathBuf, Duration)> = steps
        .seen
        .iter()
        .map(|seen| {
            let args = seen.args.iter().map(String::as_str).collect();
            (seen.program.clone(), args, seen.cwd.clone(), seen.timeout)
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                PathBuf::from("cargo"),
                vec!["nextest", "run", "--workspace", "--no-tests=fail"],
                pr.head_tree.clone(),
                Duration::from_mins(25)
            ),
            (
                PathBuf::from("cargo"),
                vec!["test", "--doc"],
                pr.head_tree.clone(),
                Duration::from_mins(10)
            ),
        ],
        "the test steps in file order; no lint step and no temper baseline --check"
    );
    let caps: Vec<usize> = steps
        .seen
        .iter()
        .map(|seen| seen.max_output_bytes)
        .collect();
    assert_eq!(
        caps,
        [16 << 20, 16 << 20],
        "each test step's output is capped at 16 MiB"
    );
    Ok(())
}

#[test]
fn the_steps_see_only_the_gate_environment() -> Result<(), Box<dyn Error>> {
    let mut steps = FakeSteps::default();

    outcome(&mut steps)?;

    assert_eq!(steps.ran(), ["nextest", "test"]);
    for seen in &steps.seen {
        let names: Vec<&OsString> = seen.env.iter().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            [
                "PATH",
                "GIT_CONFIG_NOSYSTEM",
                "GIT_CONFIG_GLOBAL",
                "GIT_CEILING_DIRECTORIES"
            ],
            "{:?}: the runner's GITHUB_TOKEN never reaches a test",
            seen.args
        );
    }
    Ok(())
}

#[test]
fn passing_steps_pass_the_gate_without_a_ratchet_base() -> Result<(), Box<dyn Error>> {
    let mut steps = FakeSteps::default();

    let (outcome, document) = outcome(&mut steps)?;

    let document = document.ok_or("a pass carries its document")?;
    assert_eq!(outcome, PASSED);
    assert_eq!(
        (document.source.name.as_str(), document.source.version),
        ("temper", format!("git:{GIT}"))
    );
    assert!(document.findings.is_empty(), "{:?}", document.findings);
    Ok(())
}

#[test]
fn zero_tests_fail_the_gate() -> Result<(), Box<dyn Error>> {
    // nextest's exit code when `--no-tests=fail` finds no test to run.
    let mut steps = FakeSteps::default().ends("nextest", &[Termination::Exited(4)]);

    let (outcome, _) = outcome(&mut steps)?;

    assert_eq!(outcome, FAILED);
    assert_eq!(steps.ran(), ["nextest", "test"], "an exit is never retried");
    Ok(())
}

#[test]
fn failing_tests_fail_the_gate_and_the_doctests_still_run() -> Result<(), Box<dyn Error>> {
    for (step, code) in [("nextest", 100), ("nextest", 101), ("test", 101)] {
        let mut steps = FakeSteps::default().ends(step, &[Termination::Exited(code)]);

        let (outcome, _) = outcome(&mut steps)?;

        assert_eq!(outcome, FAILED, "{step} exit {code}");
        assert_eq!(
            steps.ran(),
            ["nextest", "test"],
            "{step} exit {code}: a test failure is not infra-class, so it is never retried"
        );
    }
    Ok(())
}

#[test]
fn a_step_a_signal_ended_is_retried_once() -> Result<(), Box<dyn Error>> {
    for (step, ran) in [
        ("nextest", ["nextest", "nextest", "test"]),
        ("test", ["nextest", "test", "test"]),
    ] {
        let mut steps = FakeSteps::default().ends(step, &[SIGNALLED, Termination::Exited(0)]);

        let (outcome, _) = outcome(&mut steps)?;

        assert_eq!(outcome, PASSED, "{step}");
        assert_eq!(steps.ran(), ran, "{step}");
    }
    Ok(())
}

#[test]
fn the_retry_decides_the_step() -> Result<(), Box<dyn Error>> {
    let mut steps = FakeSteps::default().ends("nextest", &[SIGNALLED, Termination::Exited(100)]);

    let (outcome, _) = outcome(&mut steps)?;

    assert_eq!(outcome, FAILED);
    assert_eq!(steps.ran(), ["nextest", "nextest", "test"]);
    Ok(())
}

#[test]
fn a_step_signalled_twice_is_a_gate_error_and_stops_the_run() -> Result<(), Box<dyn Error>> {
    let mut steps = FakeSteps::default().ends("nextest", &[SIGNALLED, SIGNALLED, SIGNALLED]);

    let (outcome, _) = outcome(&mut steps)?;

    assert_eq!(outcome, GateJobOutcome::Error(GateError::Signal));
    assert_eq!(steps.ran(), ["nextest", "nextest"], "one retry, then stop");
    Ok(())
}

#[test]
fn a_timeout_or_an_output_cap_is_a_gate_error_and_never_retried() -> Result<(), Box<dyn Error>> {
    let cases = [
        (Termination::TimedOut, GateError::Timeout),
        (Termination::OutputTooLarge, GateError::OutputTooLarge),
    ];
    for (termination, error) in cases {
        let mut steps =
            FakeSteps::default().ends("nextest", &[termination, Termination::Exited(0)]);

        let (outcome, _) = outcome(&mut steps)?;

        assert_eq!(outcome, GateJobOutcome::Error(error), "{termination:?}");
        assert_eq!(steps.ran(), ["nextest"], "{termination:?}");
    }
    Ok(())
}

#[test]
fn a_step_that_cannot_start_is_a_gate_error_and_never_retried() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let config = config()?;
    let temper = plugin(&config, "temper")?;
    let mut steps = FakeSteps {
        cannot_start: Some("nextest".to_owned()),
        ..FakeSteps::default()
    };
    let mut adapter = TemperAdapter::new(&mut steps, &pr.revisions, pr.settings()?);

    let ran = adapter.run(&temper, TEST);

    assert_matches!(ran, Err(TemperError::Lint { ref process, .. }) if process == "tests");
    assert_eq!(steps.ran(), ["nextest"]);
    Ok(())
}
