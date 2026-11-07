//! Fixtures for the Weigh-In adapter (#372): a scripted `weighin ci-check` that prints a
//! `weighin.ci-check/1` report, report items and violations shaped as weighin 1.2.0 writes them,
//! and the adapter run directly or through the gate use case, so its document is
//! schema-checked.

use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;

use review_app::{
    ContractValidator, GateJob, GateJobOutcome, GateRun, GateRunner, Plugin, PluginsConfig,
    Termination, run_gates,
};
use review_domain::{Finding, FindingsDocument, Severity};
use review_gates::{
    CaptureSupervisor, CapturedRun, CiCheck, GateEnvironment, ProcessSpec, WeighInAdapter,
    WeighInError, WeighInSettings,
};
use serde_json::{Value, json};

use crate::support::SeenSpec;
use crate::temper_support::FakeError;

pub(crate) const BASE: &str = "6c51ad64e08cdf6d8ffa0ffd06dd427776318acc";
pub(crate) const HEAD: &str = "7133cd8fbaaaef75ce9a694312d2784946570a96";
pub(crate) const PROGRAM: &str = "/opt/weighin/bin/weighin";
pub(crate) const TREE: &str = "/scratch/weigh-in";
pub(crate) const STORE: &str = "/scratch/weigh-in-store/ledger.jsonl";
const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";
const GIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Base config with a weigh-in (`required` as given, a 45 s timeout) and a temper.
pub(crate) fn config(required: bool) -> Result<PluginsConfig, Box<dyn Error>> {
    Ok(format!(
        r#"
[assay]
digest = "sha256:{HEX64}"

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "git:{GIT}"
required = true

[[plugin]]
name = "weigh-in"
adapter = "builtin:weigh-in"
pin = "git:{GIT}"
required = {required}
timeout_s = 45
"#
    )
    .parse()?)
}

/// The plugin named `name` in `config`.
pub(crate) fn plugin(config: &PluginsConfig, name: &str) -> Result<Plugin, Box<dyn Error>> {
    config
        .plugins()
        .iter()
        .find(|plugin| plugin.name().as_str() == name)
        .cloned()
        .ok_or_else(|| format!("no plugin {name}").into())
}

/// An open item as `ci-check` lists it: `id` at `status`, triaged `door` and `authority`
/// (null before triage), and `blocks`.
pub(crate) fn item(
    id: &str,
    status: &str,
    (door, authority): (Option<&str>, Option<&str>),
    blocks: &str,
) -> Value {
    json!({
        "id": id,
        "status": status,
        "kind": "question",
        "door": door,
        "authority": authority,
        "blocks": blocks,
        "resolve_by": "before-merge: SECRET-DEADLINE",
        "question": "SECRET-QUESTION [x](https://e)?",
    })
}

/// A journal violation at `path`, `line` (null for a removed journal), for `reason`.
pub(crate) fn violation(path: &str, line: Option<u64>, reason: &str) -> Value {
    json!({ "path": path, "line": line, "reason": reason })
}

/// A report about [`BASE`] and [`HEAD`] with `items` and `violations`.
pub(crate) fn report(items: &[Value], violations: &[Value]) -> Vec<u8> {
    let report = json!({
        "schema": "weighin.ci-check/1",
        "base": BASE,
        "head": HEAD,
        "open_items": items,
        "decisions": [
            { "id": "wi-0000e5", "option": "B", "commit": HEAD, "sources": ["trailer"] }
        ],
        "journal": { "ok": violations.is_empty(), "violations": violations },
    });
    serde_json::to_vec(&report).unwrap_or_default()
}

/// Plays `weighin ci-check`: records what it was asked to run, then ends with `termination`
/// having printed `stdout`, or fails as a supervisor when `fails`.
#[derive(Debug)]
pub(crate) struct FakeCiCheck {
    pub(crate) termination: Termination,
    pub(crate) stdout: Vec<u8>,
    pub(crate) fails: bool,
    pub(crate) seen: Vec<SeenSpec>,
}

impl FakeCiCheck {
    /// A run that exits 0 having printed `stdout`.
    pub(crate) fn printing(stdout: Vec<u8>) -> Self {
        Self {
            termination: Termination::Exited(0),
            stdout,
            fails: false,
            seen: Vec::new(),
        }
    }
}

impl CaptureSupervisor for FakeCiCheck {
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
        if self.fails {
            return Err(FakeError);
        }
        Ok(CapturedRun {
            termination: self.termination,
            stdout: self.stdout.clone(),
        })
    }
}

/// The gate environment the tests pass: an absolute `PATH` and a `HOME`.
pub(crate) fn environment() -> GateEnvironment {
    GateEnvironment::from_vars([
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("HOME"), OsString::from("/home/runner")),
        (OsString::from("REVIEW_RUN_TOKEN"), OsString::from("ghs_x")),
    ])
}

/// What the adapter runs on: [`PROGRAM`] in [`TREE`] with [`STORE`], about [`BASE`] and
/// [`HEAD`].
pub(crate) fn check() -> Result<CiCheck, Box<dyn Error>> {
    Ok(CiCheck {
        program: PathBuf::from(PROGRAM),
        tree: PathBuf::from(TREE),
        store: PathBuf::from(STORE),
        base: BASE.parse()?,
        head: HEAD.parse()?,
    })
}

/// The adapter's result with the fake.
pub(crate) type Ran = Result<GateRun, WeighInError<FakeError>>;

/// What the tests compare of each finding: rule, file, line and severity.
pub(crate) type Seen = (String, String, u64, Severity);

/// Runs `plugin` of `config(required)` in `job` directly with `ci_check`.
pub(crate) fn run(
    ci_check: &mut FakeCiCheck,
    (required, name): (bool, &str),
    job: GateJob,
) -> Result<Ran, Box<dyn Error>> {
    let config = config(required)?;
    let plugin = plugin(&config, name)?;
    let settings = WeighInSettings::new(check()?, &environment(), "run-1")?;
    let mut adapter = WeighInAdapter::new(ci_check, settings);
    Ok(adapter.run(&plugin, job))
}

/// What [`WeighInAdapter::admit`] says of the plugin `name` (required or not) in `job`, for
/// an adapter over [`FakeCiCheck`].
pub(crate) fn admit(
    (required, name): (bool, &str),
    job: GateJob,
) -> Result<Result<(), WeighInError<FakeError>>, Box<dyn Error>> {
    let config = config(required)?;
    let plugin = plugin(&config, name)?;
    Ok(WeighInAdapter::<&mut FakeCiCheck>::admit(&plugin, job))
}

/// Runs `gates-static` through the gate use case with `ci_check` printing `stdout` (temper,
/// which this adapter refuses, is dropped): the weigh-in outcome and the validated document.
pub(crate) fn document(
    stdout: Vec<u8>,
) -> Result<(GateJobOutcome, Option<FindingsDocument>), Box<dyn Error>> {
    let config = config(false)?;
    let settings = WeighInSettings::new(check()?, &environment(), "run-1")?;
    let mut adapter = WeighInAdapter::new(FakeCiCheck::printing(stdout), settings);
    let validator = ContractValidator::new()?;
    let reports = run_gates(&config, GateJob::Static, &mut adapter, &validator);
    let report = reports
        .into_iter()
        .find(|report| report.plugin.as_str() == "weigh-in")
        .ok_or("no weigh-in report")?;
    let document = report.document.map(serde_json::from_value).transpose()?;
    Ok((report.outcome, document))
}

/// The outcome and the findings of a `gates-static` run over `stdout`.
pub(crate) fn findings(stdout: Vec<u8>) -> Result<(GateJobOutcome, Vec<Finding>), Box<dyn Error>> {
    let (outcome, document) = document(stdout)?;
    Ok((outcome, document.map(|d| d.findings).unwrap_or_default()))
}

/// The outcome and the rule, file, line and severity of each finding over `stdout`.
pub(crate) fn seen(stdout: Vec<u8>) -> Result<(GateJobOutcome, Vec<Seen>), Box<dyn Error>> {
    let (outcome, findings) = findings(stdout)?;
    let seen = findings
        .into_iter()
        .map(|finding| {
            let rule = finding.rule.unwrap_or_default();
            (rule, finding.file, finding.line.get(), finding.severity)
        })
        .collect();
    Ok((outcome, seen))
}
