//! The temper adapter in `gates-lint` (#66, `docs/architecture/03-contracts.md` §2.1 steps 5-7):
//! the pinned `baseline --check` with its ratchet asserted, `raised` and `exception` entries
//! unioned into failures, display findings, and the declared `fmt` and `deny` steps.
//!
//! A scripted supervisor plays temper and the steps. The documents it leaves are temper 1.3.0's
//! `--findings-out` output: `tests/fixtures/temper-check-strict.json` is a real recording (this
//! repository at d44a0ad, which has no baseline); `temper-check-diff-totals.json` and
//! `temper-check-git-diff-failed.json` are written from temper 1.3.0's `collectFindings` and
//! `ratchet` (`lib/findings.mjs`, `lib/baseline.mjs`), the latter as `ratchet` leaves it when
//! `git diff` throws: mode `totals`, base `null`, `ok` still true.

use std::assert_matches;
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{
    CommitId, ContractValidator, GateError, GateJob, GateJobOutcome, GateRun, GateRunner,
    PluginsConfig, Termination, run_gates,
};
use assay_domain::{FindingsDocument, GateConclusion, Severity, Side};
use assay_gates::{
    CaptureSupervisor, CapturedRun, ProcessSpec, RatchetBase, RatchetDegradation, Revision,
    TemperError,
};
use serde_json::Value;

use crate::support::{GIT, SeenSpec};
use crate::temper_support::{CONFIG, EXCEPTIONS, NODE, Pr};

const LINT: GateJob = GateJob::Build(assay_app::BuildJob::Lint);
const BASE_SHA: &str = "ba5e000000000000000000000000000000000001";
const MERGE_BASE: &str = "5e1f0b9c2d3a4e5f60718293a4b5c6d7e8f90a1b";
const FINDINGS_OUT: &str = "target/temper/findings.json";
const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

const DIFF_TOTALS: &str = include_str!("../fixtures/temper-check-diff-totals.json");
const GIT_DIFF_FAILED: &str = include_str!("../fixtures/temper-check-git-diff-failed.json");
const STRICT: &str = include_str!("../fixtures/temper-check-strict.json");

/// temper with the template's `fmt` and `deny` lint steps and a `tests` step in gates-test.
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
cmd = ["cargo", "fmt", "--all", "--", "--check"]
timeout_s = 120

[[plugin.step]]
name = "deny"
job = "lint"
cmd = ["cargo", "deny", "check"]
timeout_s = 300

[[plugin.step]]
name = "tests"
job = "test"
cmd = ["cargo", "nextest", "run", "--no-tests=fail"]
timeout_s = 600
"#
    )
    .parse()?)
}

/// What the scripted baseline run leaves at `target/temper/findings.json`.
#[derive(Debug, Clone)]
enum Leaves {
    Nothing,
    File(Vec<u8>),
    SymlinkTo(PathBuf),
}

#[derive(Debug)]
struct FakeError(String);

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for FakeError {}

/// Plays the pinned `temper baseline --check` and the declared steps.
#[derive(Debug)]
struct FakeLint {
    baseline: Termination,
    leaves: Leaves,
    /// How each step ends, by its program's first argument (`fmt`, `deny`); exit 0 otherwise.
    steps: BTreeMap<String, Termination>,
    /// How a step's first attempts end, in order, before `steps` applies.
    attempts: BTreeMap<String, VecDeque<Termination>>,
    /// When set, the supervisor cannot start this step, or `baseline --check` for `baseline`.
    cannot_start: Option<String>,
    seen: Vec<SeenSpec>,
}

impl FakeLint {
    fn leaving(document: &str) -> Self {
        Self::ending(
            Termination::Exited(0),
            Leaves::File(document.as_bytes().to_vec()),
        )
    }

    fn ending(baseline: Termination, leaves: Leaves) -> Self {
        Self {
            baseline,
            leaves,
            steps: BTreeMap::new(),
            attempts: BTreeMap::new(),
            cannot_start: None,
            seen: Vec::new(),
        }
    }

    fn step_ends(mut self, step: &str, termination: Termination) -> Self {
        self.steps.insert(step.to_owned(), termination);
        self
    }

    /// `step`'s first attempts end as `attempts`, in order; later ones as `steps` says.
    fn step_attempts(mut self, step: &str, attempts: &[Termination]) -> Self {
        self.attempts
            .insert(step.to_owned(), attempts.iter().copied().collect());
        self
    }

    /// Each process run, in order: a step by its first argument, `baseline --check` as
    /// `baseline`.
    fn ran(&self) -> Vec<&str> {
        self.seen
            .iter()
            .map(|seen| {
                if seen.args.get(1).map(String::as_str) == Some("baseline") {
                    "baseline"
                } else {
                    seen.args.first().map_or("", String::as_str)
                }
            })
            .collect()
    }

    fn play_baseline(&self, cwd: &Path) -> Result<Termination, FakeError> {
        let out = cwd.join(FINDINGS_OUT);
        let failed = |error: std::io::Error| FakeError(error.to_string());
        match &self.leaves {
            Leaves::Nothing => {}
            Leaves::File(bytes) => {
                fs::create_dir_all(cwd.join("target/temper")).map_err(failed)?;
                fs::write(&out, bytes).map_err(failed)?;
            }
            Leaves::SymlinkTo(target) => {
                fs::create_dir_all(cwd.join("target/temper")).map_err(failed)?;
                std::os::unix::fs::symlink(target, &out).map_err(failed)?;
            }
        }
        Ok(self.baseline)
    }
}

impl CaptureSupervisor for FakeLint {
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
        let baseline = spec.args.get(1).map(String::as_str) == Some("baseline");
        let first = spec.args.first().map(String::as_str).unwrap_or_default();
        let process = if baseline { "baseline" } else { first };
        let termination = if self.cannot_start.as_deref() == Some(process) {
            return Err(FakeError(format!("cannot start {process}")));
        } else if baseline {
            self.play_baseline(spec.cwd)?
        } else if let Some(attempt) = self.attempts.get_mut(first).and_then(VecDeque::pop_front) {
            attempt
        } else {
            self.steps
                .get(first)
                .copied()
                .unwrap_or(Termination::Exited(0))
        };
        Ok(CapturedRun {
            termination,
            stdout: Vec::new(),
        })
    }
}

fn commit(text: &str) -> Result<CommitId, Box<dyn Error>> {
    Ok(text.parse()?)
}

fn ratchet_base() -> Result<RatchetBase, Box<dyn Error>> {
    Ok(RatchetBase {
        base: commit(BASE_SHA)?,
        merge_base: commit(MERGE_BASE)?,
    })
}

/// Runs the temper plugin in gates-lint directly.
fn run(pr: &Pr, lint: &mut FakeLint) -> Result<RanLint, Box<dyn Error>> {
    let config = config()?;
    let temper = crate::support::plugin(&config, "temper")?;
    let settings = pr.settings()?.with_ratchet(ratchet_base()?);
    let mut adapter = assay_gates::TemperAdapter::new(lint, &pr.revisions, settings);
    Ok(adapter.run(&temper, LINT))
}

type RanLint = Result<GateRun, TemperError<FakeError, crate::temper_support::FakeError>>;

/// Runs gates-lint through the gate use case: the outcome and the validated document.
fn outcome(
    pr: &Pr,
    lint: &mut FakeLint,
) -> Result<(GateJobOutcome, Option<FindingsDocument>), Box<dyn Error>> {
    let config = config()?;
    let settings = pr.settings()?.with_ratchet(ratchet_base()?);
    let mut adapter = assay_gates::TemperAdapter::new(lint, &pr.revisions, settings);
    let reports = run_gates(&config, LINT, &mut adapter, &ContractValidator::new()?);
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

/// The rule, file, line, side, severity and evidence of each finding.
type Shown = (String, String, u64, Option<Side>, Severity, String);

fn shown(document: Option<FindingsDocument>) -> Vec<Shown> {
    document
        .map(|document| {
            document
                .findings
                .into_iter()
                .map(|finding| {
                    (
                        finding.rule.unwrap_or_default(),
                        finding.file,
                        finding.line.get(),
                        finding.side,
                        finding.severity,
                        finding.evidence,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The diff+totals fixture with `edit` applied.
fn edited(edit: impl FnOnce(&mut Value) -> Option<()>) -> Result<String, Box<dyn Error>> {
    let mut document: Value = serde_json::from_str(DIFF_TOTALS)?;
    edit(&mut document).ok_or("the fixture lacks a field the test edits")?;
    Ok(format!("{document:#}\n"))
}

fn ratchet(document: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    document.get_mut("ratchet")?.as_object_mut()
}

fn push(document: &mut Value, key: &str, item: Value) -> Option<()> {
    document.get_mut(key)?.as_array_mut()?.push(item);
    Some(())
}

/// A clippy finding as temper's `messageParser` records it.
fn clippy(lint: &str, (file, number): (&str, u64), severity: &str) -> Value {
    serde_json::json!({
        "tool": "clippy",
        "lint": lint,
        "crate": "fixture",
        "file": file,
        "line": number,
        "end_line": number,
        "column": 5,
        "message": "a lint message",
        "rules": ["H-02"],
        "severity": severity,
    })
}

/// A finding on a changed line: in the collection and in `ratchet.new_code`, with `ok` false.
fn new_code(document: &mut Value, finding: &Value) -> Option<()> {
    push(document, "findings", finding.clone())?;
    let ratchet = ratchet(document)?;
    ratchet.insert("ok".to_owned(), false.into());
    ratchet
        .get_mut("new_code")?
        .as_array_mut()?
        .push(finding.clone());
    Some(())
}

const PASSED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Pass);
const FAILED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Fail);

#[test]
fn a_clean_ratchet_against_the_merge_base_passes() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    let (outcome, document) = outcome(&pr, &mut lint)?;

    assert_eq!((outcome, shown(document)), (PASSED, Vec::new()));
    Ok(())
}

#[test]
fn runs_the_pinned_baseline_check_against_the_base_sha_from_the_head_tree()
-> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    run(&pr, &mut lint)??;

    let script = pr.pinned.join("temper.mjs").display().to_string();
    let first = lint.seen.last().ok_or("nothing ran")?;
    assert_eq!(
        (&first.program, &first.args, &first.cwd, first.timeout),
        (
            &PathBuf::from(NODE),
            &[
                script.as_str(),
                "baseline",
                "--check",
                "--locked",
                "--base",
                BASE_SHA,
                "--findings-out",
                FINDINGS_OUT,
            ]
            .map(str::to_owned)
            .to_vec(),
            &pr.head_tree,
            Duration::from_mins(20),
        )
    );
    let names: Vec<&OsString> = first.env.iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        [
            "PATH",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CEILING_DIRECTORIES"
        ],
        "the allowlist plus git hardening; no GITHUB_TOKEN"
    );
    Ok(())
}

#[test]
fn refuses_gates_lint_without_a_ratchet_base_before_running_anything() -> Result<(), Box<dyn Error>>
{
    let pr = Pr::new()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);
    let config = config()?;
    let temper = crate::support::plugin(&config, "temper")?;
    let mut adapter = assay_gates::TemperAdapter::new(&mut lint, &pr.revisions, pr.settings()?);

    let result = adapter.run(&temper, LINT);

    assert_matches!(result, Err(TemperError::NoRatchetBase));
    assert_eq!(lint.seen, Vec::new(), "nothing ran");
    Ok(())
}

#[test]
fn a_ratchet_whose_git_diff_failed_inside_temper_is_degraded() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;

    let ran = run(&pr, &mut FakeLint::leaving(GIT_DIFF_FAILED))?;
    let (outcome, _) = outcome(&pr, &mut FakeLint::leaving(GIT_DIFF_FAILED))?;

    assert_matches!(
        ran,
        Err(TemperError::RatchetDegraded {
            why: RatchetDegradation::Mode
        })
    );
    assert_eq!(
        outcome,
        GateJobOutcome::Error(GateError::RatchetDegraded),
        "temper said ok, but compared only the totals"
    );
    Ok(())
}

#[test]
fn a_totals_only_note_is_degraded_whatever_the_mode() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let document = edited(|document| {
        let note = "git diff against 5e1f0b9c2d3a failed; only the totals were compared";
        ratchet(document)?.insert("notes".to_owned(), serde_json::json!([note]));
        Some(())
    })?;

    let ran = run(&pr, &mut FakeLint::leaving(&document))?;

    assert_matches!(
        ran,
        Err(TemperError::RatchetDegraded {
            why: RatchetDegradation::TotalsNote
        })
    );
    Ok(())
}

#[test]
fn a_strict_ratchet_without_a_baseline_is_degraded() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;

    let ran = run(&pr, &mut FakeLint::leaving(STRICT))?;

    assert_matches!(
        ran,
        Err(TemperError::RatchetDegraded {
            why: RatchetDegradation::Mode
        }),
        "strict compares no diff, so it is not the ratchet 03 §2.1 asserts"
    );
    Ok(())
}

#[test]
fn an_unknown_mode_is_degraded() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let document = edited(|document| {
        ratchet(document)?.insert("mode".to_owned(), "diff".into());
        Some(())
    })?;

    let ran = run(&pr, &mut FakeLint::leaving(&document))?;

    assert_matches!(
        ran,
        Err(TemperError::RatchetDegraded {
            why: RatchetDegradation::Mode
        })
    );
    Ok(())
}

#[test]
fn a_ratchet_on_another_base_or_none_is_degraded() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    for base in [
        Value::from(BASE_SHA),
        Value::Null,
        Value::from("5e1f0b9c2d3a"),
    ] {
        let document = edited(|document| {
            ratchet(document)?.insert("base".to_owned(), base.clone());
            Some(())
        })?;

        let ran = run(&pr, &mut FakeLint::leaving(&document))?;

        assert_matches!(
            ran,
            Err(TemperError::RatchetDegraded {
                why: RatchetDegradation::Base
            }),
            "{base}"
        );
    }
    Ok(())
}

/// The diff+totals fixture collected from `sources` only.
fn collected_from(sources: &[&str]) -> Result<String, Box<dyn Error>> {
    edited(|document| {
        document
            .as_object_mut()?
            .insert("sources".to_owned(), Value::from(sources.to_vec()));
        Some(())
    })
}

/// #210 F3: a head `.claude/temper.json` that turns rustdoc or the scan off makes temper ratchet
/// without them while it still reports `diff+totals` and `ok`; a collection that leaves out a
/// source base enables is not believed.
#[test]
fn a_ratchet_without_a_source_base_enables_is_degraded() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    for sources in [
        &["clippy", "scan"][..],
        &["clippy", "rustdoc"][..],
        &["rustdoc", "scan"][..],
        &[][..],
    ] {
        let document = collected_from(sources)?;

        let ran = run(&pr, &mut FakeLint::leaving(&document))?;
        let (outcome, _) = outcome(&pr, &mut FakeLint::leaving(&document))?;

        assert_matches!(
            ran,
            Err(TemperError::RatchetDegraded {
                why: RatchetDegradation::Sources
            }),
            "{sources:?}"
        );
        assert_eq!(outcome, GateJobOutcome::Error(GateError::RatchetDegraded));
    }
    Ok(())
}

#[test]
fn a_ratchet_without_the_sources_base_turns_off_is_believed() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Base,
        CONFIG,
        r#"{"gate": {"doc": false, "scan": false}}"#,
    );
    let mut lint = FakeLint::leaving(&collected_from(&["clippy"])?);

    let (outcome, _) = outcome(&pr, &mut lint)?;

    assert_eq!(outcome, PASSED, "base enables only clippy");
    Ok(())
}

/// With no `.claude/temper.json` at base, temper's defaults enable every source.
#[test]
fn without_a_base_config_every_source_is_required() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.remove(Revision::Base, CONFIG);
    let document = collected_from(&["clippy", "scan"])?;

    let ran = run(&pr, &mut FakeLint::leaving(&document))?;
    let (clean, _) = outcome(&pr, &mut FakeLint::leaving(DIFF_TOTALS))?;

    assert_matches!(
        ran,
        Err(TemperError::RatchetDegraded {
            why: RatchetDegradation::Sources
        })
    );
    assert_eq!(clean, PASSED);
    Ok(())
}

/// An unreadable base config is refused before any step or `baseline --check` runs.
#[test]
fn a_base_config_that_is_not_a_json_object_is_refused_before_anything_runs()
-> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, CONFIG, "[]");
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    let ran = run(&pr, &mut lint)?;

    assert_matches!(
        ran,
        Err(TemperError::Malformed {
            revision: Revision::Base,
            path: ".claude/temper.json",
            ..
        })
    );
    assert_eq!(lint.ran(), Vec::<&str>::new(), "nothing ran");
    Ok(())
}

/// Only an exact JSON `false` turns a source off, as temper's `cfg.gate.doc !== false`.
#[test]
fn a_base_config_value_other_than_false_keeps_the_source_on() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Base,
        CONFIG,
        r#"{"gate": {"doc": 0, "scan": "false"}}"#,
    );
    let document = collected_from(&["clippy"])?;

    let ran = run(&pr, &mut FakeLint::leaving(&document))?;

    assert_matches!(
        ran,
        Err(TemperError::RatchetDegraded {
            why: RatchetDegradation::Sources
        })
    );
    Ok(())
}

#[test]
fn unexpected_findings_output_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let repeated = DIFF_TOTALS.replacen("\"ok\": true,", "\"ok\": true,\n    \"ok\": false,", 1);
    let cases = [
        ("repeated key", repeated),
        ("not JSON", "{\"temper_findings\": 1".to_owned()),
        (
            "unknown top-level field",
            edited(|document| {
                document
                    .as_object_mut()?
                    .insert("extra".to_owned(), 1.into());
                Some(())
            })?,
        ),
        (
            "unknown ratchet field",
            edited(|document| {
                ratchet(document)?.insert("extra".to_owned(), 1.into());
                Some(())
            })?,
        ),
        (
            "unknown severity",
            edited(|document| {
                push(
                    document,
                    "findings",
                    clippy("clippy::x", ("src/lib.rs", 1), "high"),
                )
            })?,
        ),
        (
            "another format",
            edited(|document| {
                document
                    .as_object_mut()?
                    .insert("temper_findings".to_owned(), 2.into());
                Some(())
            })?,
        ),
        (
            "line zero",
            edited(|document| {
                push(
                    document,
                    "findings",
                    clippy("clippy::x", ("src/lib.rs", 0), "must"),
                )
            })?,
        ),
    ];
    for (case, document) in cases {
        let ran = run(&pr, &mut FakeLint::leaving(&document))?;

        assert_matches!(ran, Err(TemperError::FindingsOutput { .. }), "{case}");
    }
    Ok(())
}

#[test]
fn no_findings_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let mut lint = FakeLint::ending(Termination::Exited(0), Leaves::Nothing);

    let ran = run(&pr, &mut lint)?;

    assert_matches!(ran, Err(TemperError::FindingsOutput { .. }));
    Ok(())
}

#[test]
fn a_stale_findings_document_is_removed_before_the_run() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    fs::create_dir_all(pr.head_tree.join("target/temper"))?;
    fs::write(pr.head_tree.join(FINDINGS_OUT), DIFF_TOTALS)?;
    let mut lint = FakeLint::ending(Termination::Exited(0), Leaves::Nothing);

    let ran = run(&pr, &mut lint)?;

    assert_matches!(
        ran,
        Err(TemperError::FindingsOutput { .. }),
        "the document a warm target/ or an earlier run left is never read"
    );
    Ok(())
}

#[test]
fn a_symlinked_findings_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let elsewhere = pr.head_tree.join("elsewhere.json");
    fs::write(&elsewhere, DIFF_TOTALS)?;
    let mut lint = FakeLint::ending(Termination::Exited(0), Leaves::SymlinkTo(elsewhere));

    let ran = run(&pr, &mut lint)?;

    assert_matches!(ran, Err(TemperError::FindingsOutput { .. }));
    Ok(())
}

#[test]
fn an_exit_code_that_disagrees_with_the_ratchet_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let failing = edited(|document| {
        new_code(
            document,
            &clippy("clippy::unwrap_used", ("src/lib.rs", 2), "must"),
        )
    })?;
    let cases = [
        (Termination::Exited(1), DIFF_TOTALS.to_owned()),
        (Termination::Exited(0), failing),
    ];
    for (termination, document) in cases {
        let mut lint = FakeLint::ending(termination, Leaves::File(document.into_bytes()));

        let ran = run(&pr, &mut lint)?;

        assert_matches!(
            ran,
            Err(TemperError::FindingsOutput { .. }),
            "{termination:?}"
        );
    }
    Ok(())
}

#[test]
fn a_baseline_run_that_does_not_exit_0_or_1_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let cases = [
        (Termination::Exited(2), GateError::Exit(2)),
        (Termination::TimedOut, GateError::Timeout),
        (Termination::Signalled, GateError::Signal),
        (Termination::OutputTooLarge, GateError::OutputTooLarge),
    ];
    for (termination, expected) in cases {
        let mut lint = FakeLint::ending(termination, Leaves::Nothing);

        let (outcome, _) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, GateJobOutcome::Error(expected), "{termination:?}");
    }
    Ok(())
}

#[test]
fn a_finding_on_a_changed_line_fails_with_the_head_line_as_evidence() -> Result<(), Box<dyn Error>>
{
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "pub fn one(x: Option<u8>) -> u8 {\n    x.unwrap()\n}\n",
    );
    let document = edited(|document| {
        new_code(
            document,
            &clippy("clippy::unwrap_used", ("src/lib.rs", 2), "must"),
        )
    })?;
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let (outcome, document) = outcome(&pr, &mut lint)?;

    assert_eq!(
        (outcome, shown(document)),
        (
            FAILED,
            vec![(
                "clippy::unwrap_used".to_owned(),
                "src/lib.rs".to_owned(),
                2,
                Some(Side::Right),
                Severity::Blocker,
                "x.unwrap()".to_owned(),
            )]
        )
    );
    Ok(())
}

#[test]
fn temper_severities_map_to_display_severities() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.rs", "a\nb\nc\nd\n");
    let document = edited(|document| {
        for (line, severity) in [(1, "must"), (2, "should"), (3, "consider"), (4, "unmapped")] {
            new_code(
                document,
                &clippy("clippy::x", ("src/lib.rs", line), severity),
            )?;
        }
        Some(())
    })?;
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let (_, document) = outcome(&pr, &mut lint)?;

    let severities: Vec<Severity> = shown(document).into_iter().map(|seen| seen.4).collect();
    assert_eq!(
        severities,
        [
            Severity::Blocker,
            Severity::Major,
            Severity::Minor,
            Severity::Minor
        ]
    );
    Ok(())
}

#[test]
fn a_head_file_shorter_than_the_cited_line_is_never_quoted_from_base() -> Result<(), Box<dyn Error>>
{
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.rs", "fn a() {}\n");
    pr.set(
        Revision::Base,
        "src/lib.rs",
        "fn a() {}\nfn b() { x.unwrap() }\n",
    );
    let document = edited(|document| {
        new_code(
            document,
            &clippy("clippy::unwrap_used", ("src/lib.rs", 2), "must"),
        )
    })?;
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let (_, document) = outcome(&pr, &mut lint)?;

    let sides: Vec<(Option<Side>, String)> = shown(document)
        .into_iter()
        .map(|seen| (seen.3, seen.5))
        .collect();
    assert_eq!(
        sides,
        [(Some(Side::Right), "clippy::unwrap_used".to_owned())],
        "base is read only for a file head deleted (03 §2.1 step 6); this one falls back"
    );
    Ok(())
}

#[test]
fn evidence_comes_from_base_when_head_deleted_the_file() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Base,
        "src/gone.rs",
        "fn a() {}\nfn b() { x.unwrap() }\n",
    );
    let document = edited(|document| {
        new_code(
            document,
            &clippy("clippy::unwrap_used", ("src/gone.rs", 2), "must"),
        )
    })?;
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let (_, document) = outcome(&pr, &mut lint)?;

    let sides: Vec<(Option<Side>, String)> = shown(document)
        .into_iter()
        .map(|seen| (seen.3, seen.5))
        .collect();
    assert_eq!(
        sides,
        [(Some(Side::Left), "fn b() { x.unwrap() }".to_owned())]
    );
    Ok(())
}

#[test]
fn a_count_above_the_baseline_is_shown_at_the_baseline_with_its_lints_severity()
-> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let document = edited(|document| {
        let ratchet = ratchet(document)?;
        ratchet.insert("ok".to_owned(), false.into());
        let increase = serde_json::json!({
            "crate": "fixture", "lint": "clippy::unwrap_used", "baseline": 1, "current": 2
        });
        ratchet.get_mut("increases")?.as_array_mut()?.push(increase);
        Some(())
    })?;
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let (outcome, document) = outcome(&pr, &mut lint)?;

    let seen: Vec<(String, String, Severity)> = shown(document)
        .into_iter()
        .map(|seen| (seen.0, seen.1, seen.4))
        .collect();
    assert_eq!(
        (outcome, seen),
        (
            FAILED,
            vec![(
                "clippy::unwrap_used".to_owned(),
                ".claude/temper-baseline.json".to_owned(),
                Severity::Blocker,
            )]
        ),
        "the fixture's legacy `unwrap_used` is `must`"
    );
    Ok(())
}

#[test]
fn a_located_compile_error_is_shown_as_a_blocker() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.rs", "fn broken( {\n");
    let document = edited(|document| {
        let error = serde_json::json!({
            "tool": "clippy", "code": null, "file": "src/lib.rs", "line": 1,
            "message": "this file contains an unclosed delimiter", "target": "fixture"
        });
        let unlocated = serde_json::json!({
            "tool": "metadata", "code": null, "file": null, "line": null, "message": "m"
        });
        let ratchet = ratchet(document)?;
        ratchet.insert("ok".to_owned(), false.into());
        let errors = ratchet.get_mut("errors")?.as_array_mut()?;
        errors.extend([error, unlocated]);
        Some(())
    })?;
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let (outcome, document) = outcome(&pr, &mut lint)?;

    assert_eq!(
        (outcome, shown(document)),
        (
            FAILED,
            vec![(
                "compile-error".to_owned(),
                "src/lib.rs".to_owned(),
                1,
                Some(Side::Right),
                Severity::Blocker,
                "fn broken( {".to_owned(),
            )]
        ),
        "an error without a location still fails, through ratchet.ok"
    );
    Ok(())
}

#[test]
fn a_raised_entry_fails_even_when_temper_passes() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let document = edited(|document| {
        let raise = serde_json::json!({
            "crate": "assay-forge", "lint": "clippy::too_many_lines", "from": 2, "to": 3
        });
        ratchet(document)?
            .get_mut("raised")?
            .as_array_mut()?
            .push(raise);
        Some(())
    })?;

    let (outcome, document) = outcome(&pr, &mut FakeLint::leaving(&document))?;

    let rules: Vec<String> = shown(document).into_iter().map(|seen| seen.0).collect();
    assert_eq!((outcome, rules), (FAILED, vec!["raised".to_owned()]));
    Ok(())
}

/// An `#[expect]` hit as temper's `scanFindings` records it: `exception` names the lint.
fn expect_hit(file: &str) -> Value {
    serde_json::json!({
        "tool": "scan",
        "lint": "scan:expect-attr",
        "crate": "fixture",
        "file": file,
        "line": 1,
        "end_line": 1,
        "message": "lint exception requested",
        "rules": ["H-05"],
        "severity": "must",
        "exception": "clippy::too_many_lines",
    })
}

#[test]
fn an_exception_base_does_not_admit_fails() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let document = edited(|document| push(document, "findings", expect_hit("src/lib.rs")))?;

    let (outcome, document) = outcome(&pr, &mut FakeLint::leaving(&document))?;

    let rules: Vec<String> = shown(document).into_iter().map(|seen| seen.0).collect();
    assert_eq!((outcome, rules), (FAILED, vec!["exception".to_owned()]));
    Ok(())
}

#[test]
fn an_exception_base_admits_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Base,
        EXCEPTIONS,
        "[[exception]]\npath = \"src/lib.rs\"\nlint = \"clippy::too_many_lines\"\n\
         reason = \"one state machine\"\n",
    );
    let document = edited(|document| push(document, "findings", expect_hit("src/lib.rs")))?;

    let (outcome, document) = outcome(&pr, &mut FakeLint::leaving(&document))?;

    assert_eq!((outcome, shown(document)), (PASSED, Vec::new()));
    Ok(())
}

#[test]
fn the_lint_steps_run_from_the_head_tree_under_their_own_timeouts_before_the_build()
-> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    run(&pr, &mut lint)??;

    let last = lint.seen.last().and_then(|seen| seen.args.get(1));
    assert_eq!(
        (lint.seen.len(), last.map(String::as_str)),
        (3, Some("baseline")),
        "baseline --check runs last, after the steps read the unbuilt tree"
    );
    let steps: Vec<(PathBuf, Vec<String>, &Path, Duration)> = lint
        .seen
        .iter()
        .take(2)
        .map(|seen| {
            (
                seen.program.clone(),
                seen.args.clone(),
                seen.cwd.as_path(),
                seen.timeout,
            )
        })
        .collect();
    let args = |args: &[&str]| args.iter().copied().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        steps,
        [
            (
                PathBuf::from("cargo"),
                args(&["fmt", "--all", "--", "--check"]),
                pr.head_tree.as_path(),
                Duration::from_secs(120),
            ),
            (
                PathBuf::from("cargo"),
                args(&["deny", "check"]),
                pr.head_tree.as_path(),
                Duration::from_secs(300),
            ),
        ],
        "fmt and deny in file order; the gates-test step does not run here"
    );
    Ok(())
}

#[test]
fn a_failing_fmt_or_deny_step_fails_the_gate() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    for step in ["fmt", "deny"] {
        let mut lint = FakeLint::leaving(DIFF_TOTALS).step_ends(step, Termination::Exited(1));

        let (outcome, _) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, FAILED, "{step}");
    }
    Ok(())
}

#[test]
fn a_step_past_its_timeout_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS).step_ends("deny", Termination::TimedOut);

    let (outcome, _) = outcome(&pr, &mut lint)?;

    assert_eq!(outcome, GateJobOutcome::Error(GateError::Timeout));
    Ok(())
}

#[test]
fn a_step_that_cannot_start_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);
    lint.cannot_start = Some("fmt".to_owned());

    let ran = run(&pr, &mut lint)?;

    assert_matches!(ran, Err(TemperError::Lint { .. }));
    Ok(())
}

#[test]
fn baseline_check_that_cannot_start_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);
    lint.cannot_start = Some("baseline".to_owned());

    let ran = run(&pr, &mut lint)?;

    assert_matches!(ran, Err(TemperError::Lint { ref process, .. }) if process == "temper baseline --check");
    Ok(())
}

#[test]
fn a_step_that_does_not_exit_is_that_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let cases: [(Termination, GateError, &[&str]); 2] = [
        // A signal is infra-class (06 F9): one retry, signalled again, then the run stops.
        (Termination::Signalled, GateError::Signal, &["fmt", "fmt"]),
        (
            Termination::OutputTooLarge,
            GateError::OutputTooLarge,
            &["fmt"],
        ),
    ];
    for (termination, expected, ran) in cases {
        let mut lint = FakeLint::leaving(DIFF_TOTALS).step_ends("fmt", termination);

        let (outcome, _) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, GateJobOutcome::Error(expected), "{termination:?}");
        assert_eq!(lint.ran(), ran, "{termination:?}: nothing runs after it");
    }
    Ok(())
}

#[test]
fn a_lint_step_a_signal_ended_is_retried_once_and_the_retry_decides() -> Result<(), Box<dyn Error>>
{
    let pr = Pr::new()?;
    let cases = [
        (
            "fmt",
            Termination::Exited(0),
            PASSED,
            ["fmt", "fmt", "deny", "baseline"],
        ),
        (
            "deny",
            Termination::Exited(1),
            FAILED,
            ["fmt", "deny", "deny", "baseline"],
        ),
    ];
    for (step, retry, expected, ran) in cases {
        let mut lint =
            FakeLint::leaving(DIFF_TOTALS).step_attempts(step, &[Termination::Signalled, retry]);

        let (outcome, _) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, expected, "{step}");
        assert_eq!(lint.ran(), ran, "{step}");
    }
    Ok(())
}

#[test]
fn a_lint_step_that_exits_non_zero_is_not_retried() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    for step in ["fmt", "deny"] {
        let mut lint = FakeLint::leaving(DIFF_TOTALS)
            .step_attempts(step, &[Termination::Exited(1), Termination::Exited(0)]);

        let (outcome, _) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, FAILED, "{step}");
        assert_eq!(
            lint.ran(),
            ["fmt", "deny", "baseline"],
            "{step}: an exit is the step's verdict"
        );
    }
    Ok(())
}

/// The diff+totals fixture with a finding on line 2 of `src/lib.rs`, which head holds.
fn cited_in_head(pr: &mut Pr) -> Result<String, Box<dyn Error>> {
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "fn a() {}\nfn b() { x.unwrap() }\n",
    );
    edited(|document| {
        new_code(
            document,
            &clippy("clippy::unwrap_used", ("src/lib.rs", 2), "must"),
        )
    })
}

#[test]
fn an_evidence_tree_that_cannot_be_listed_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let document = cited_in_head(&mut pr)?;
    pr.revisions.failing_tree = Some(Revision::Head);
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let ran = run(&pr, &mut lint)?;

    assert_matches!(
        ran,
        Err(TemperError::Revision {
            revision: Revision::Head,
            path: ".",
            ..
        })
    );
    Ok(())
}

#[test]
fn an_evidence_blob_that_cannot_be_read_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let document = cited_in_head(&mut pr)?;
    pr.revisions.failing_read = Some("src/lib.rs".to_owned());
    let mut lint = FakeLint::ending(Termination::Exited(1), Leaves::File(document.into_bytes()));

    let ran = run(&pr, &mut lint)?;

    assert_matches!(
        ran,
        Err(TemperError::Revision {
            revision: Revision::Head,
            path: ".",
            ..
        })
    );
    Ok(())
}

#[test]
fn failures_past_the_cap_still_fail_and_report_only_the_cap() -> Result<(), Box<dyn Error>> {
    const MAX_REPORTED: usize = 1_000;
    let pr = Pr::new()?;
    let document = edited(|document| {
        let raised = ratchet(document)?.get_mut("raised")?.as_array_mut()?;
        raised.extend((0..=MAX_REPORTED).map(|index| {
            serde_json::json!({
                "crate": format!("crate-{index}"), "lint": "clippy::too_many_lines",
                "from": 2, "to": 3
            })
        }));
        Some(())
    })?;

    let (outcome, document) = outcome(&pr, &mut FakeLint::leaving(&document))?;

    let rules: Vec<String> = shown(document).into_iter().map(|seen| seen.0).collect();
    assert_eq!(
        (
            outcome,
            rules.len(),
            rules.iter().all(|rule| rule == "raised")
        ),
        (FAILED, MAX_REPORTED, true),
        "one raised entry past the cap"
    );
    Ok(())
}
