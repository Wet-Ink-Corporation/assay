//! Fakes and fixtures for the temper adapter: PR files held in memory, a scripted `temper scan`
//! and a pinned temper copy on disk.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use assay_app::{
    ContractValidator, GateJob, GateOutcome, GateRun, GateRunner, Termination, run_gates,
};
use assay_domain::{FindingsDocument, Severity};
use assay_gates::{
    CaptureSupervisor, CapturedRun, FileContent, GateEnvironment, ProcessSpec, Revision,
    RevisionFiles, TemperAdapter, TemperError, TemperSettings, TreeFile,
};
use tempfile::TempDir;

use crate::support::{SeenSpec, config, plugin};

pub(crate) const BASELINE: &str = ".claude/temper-baseline.json";
pub(crate) const CONFIG: &str = ".claude/temper.json";
pub(crate) const EXCEPTIONS: &str = ".assay/exceptions.toml";

/// `.claude/temper.json` as this repository has it.
pub(crate) const TEMPER_JSON: &str = r#"{
  "skillGate": true,
  "review": "required",
  "askOnNewDependency": true,
  "gate": {
    "allFeatures": true,
    "doc": true,
    "scan": true,
    "stepTimeoutSec": 900
  },
  "scanner": {
    "exempt": []
  },
  "preapprovedDependencies": ["serde", "thiserror"],
  "manifests": {
    "ownLints": []
  }
}
"#;

/// The files of the pinned temper copy, in its vendored layout.
pub(crate) const PINNED: [(&str, &str); 3] = [
    ("VERSION", "1.3.0\n"),
    ("lib/scan.mjs", "export function scanSource() {}\n"),
    ("temper.mjs", "#!/usr/bin/env node\n"),
];

/// A baseline whose totals are `counts`, as `(crate, lint, count)`, laid out the way temper
/// writes it.
pub(crate) fn baseline(counts: &[(&str, &str, u64)]) -> String {
    let mut totals: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
    for (krate, lint, count) in counts {
        totals.entry(krate).or_default().insert(lint, *count);
    }
    let document = serde_json::json!({
        "temper_baseline": 1,
        "created": "2026-10-01T00:00:00.000Z",
        "updated": "2026-10-01T00:00:00.000Z",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "toolchain": "1.98.1",
        "host": "x86_64-unknown-linux-gnu",
        "crates": { "assay-forge": "crates/assay-forge", "assay-gates": "crates/assay-gates" },
        "totals": totals,
    });
    format!("{document:#}\n")
}

/// One `temper scan --all --json` hit, shaped like temper 1.3.0's `scanSource` output with
/// the `file` the CLI adds.
pub(crate) fn hit(
    file: &str,
    line: u64,
    severity: &str,
    exception: Option<&str>,
) -> serde_json::Value {
    let mut hit = serde_json::json!({
        "key": "expect-attr",
        "id": "H-05",
        "severity": severity,
        "line": line,
        "text": "#[expect(clippy::too_many_lines, reason = \"one state machine\")]",
        "msg": "lint exception requested for `clippy::too_many_lines`",
        "fix": "Exception requests need human approval (H-05).",
        "file": file,
    });
    if let (Some(lint), Some(object)) = (exception, hit.as_object_mut()) {
        object.insert("exception".to_owned(), lint.into());
    }
    hit
}

/// The PR's base and head files, in memory.
#[derive(Debug, Default)]
pub(crate) struct FakeRevisions {
    files: BTreeMap<(Revision, String), FileContent>,
}

/// The fake cannot fail; its error type exists only to satisfy the port.
#[derive(Debug)]
pub(crate) struct NeverFails;

impl fmt::Display for NeverFails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unreachable")
    }
}

impl Error for NeverFails {}

impl RevisionFiles for FakeRevisions {
    type Error = NeverFails;

    fn files(&self, revision: Revision, path: &str) -> Result<Vec<TreeFile>, NeverFails> {
        let below = format!("{path}/");
        Ok(self
            .files
            .iter()
            .filter(|((side, name), _)| {
                *side == revision && (name == path || name.starts_with(&below))
            })
            .map(|((_, name), content)| TreeFile {
                path: name.clone(),
                content: content.clone(),
            })
            .collect())
    }
}

/// Plays `temper scan --all --json`: records what it was asked to run, then ends with
/// `termination` having printed `stdout`.
#[derive(Debug)]
pub(crate) struct FakeScan {
    pub(crate) termination: Termination,
    pub(crate) stdout: Vec<u8>,
    pub(crate) seen: Vec<SeenSpec>,
}

impl CaptureSupervisor for FakeScan {
    type Error = NeverFails;

    fn capture(&mut self, spec: &ProcessSpec<'_>) -> Result<CapturedRun, NeverFails> {
        self.seen.push(SeenSpec {
            program: spec.program.to_path_buf(),
            args: spec.args.to_vec(),
            cwd: spec.cwd.to_path_buf(),
            env: spec.env.to_vec(),
            timeout: spec.timeout,
            max_output_bytes: spec.max_output_bytes,
        });
        Ok(CapturedRun {
            termination: self.termination,
            stdout: self.stdout.clone(),
        })
    }
}

/// One PR for the temper adapter: identical base and head with a baseline and a temper.json,
/// no exceptions file, a clean scan, and a pinned temper copy on disk.
pub(crate) struct Pr {
    _dir: TempDir,
    pub(crate) pinned: PathBuf,
    pub(crate) head_tree: PathBuf,
    pub(crate) revisions: FakeRevisions,
    pub(crate) scan: FakeScan,
}

/// The finding fields the tests compare: rule, file, line and severity.
pub(crate) type Seen = (String, String, u64, Severity);

impl Pr {
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let pinned = dir.path().join("pinned-temper");
        for (path, text) in PINNED {
            write(&pinned.join(path), text)?;
        }
        let head_tree = dir.path().join("head");
        fs::create_dir(&head_tree)?;
        let mut pr = Self {
            _dir: dir,
            pinned,
            head_tree,
            revisions: FakeRevisions::default(),
            scan: FakeScan {
                termination: Termination::Exited(0),
                stdout: b"[]\n".to_vec(),
                seen: Vec::new(),
            },
        };
        let counts = baseline(&[("assay-forge", "clippy::too_many_lines", 2)]);
        pr.both(BASELINE, &counts);
        pr.both(CONFIG, TEMPER_JSON);
        Ok(pr)
    }

    /// Sets `path` to `text` on `revision`.
    pub(crate) fn set(&mut self, revision: Revision, path: &str, text: &str) {
        let content = FileContent::Bytes(text.as_bytes().to_vec());
        self.revisions
            .files
            .insert((revision, path.to_owned()), content);
    }

    /// Sets `path` to `text` on both sides.
    pub(crate) fn both(&mut self, path: &str, text: &str) {
        self.set(Revision::Base, path, text);
        self.set(Revision::Head, path, text);
    }

    /// Makes `path` on `revision` something other than a regular file (a symlink).
    pub(crate) fn special(&mut self, revision: Revision, path: &str) {
        self.revisions
            .files
            .insert((revision, path.to_owned()), FileContent::NotRegular);
    }

    pub(crate) fn remove(&mut self, revision: Revision, path: &str) {
        self.revisions.files.remove(&(revision, path.to_owned()));
    }

    /// Makes the scan print `hits` as a JSON array.
    pub(crate) fn scan_prints(&mut self, hits: &[serde_json::Value]) {
        self.scan.stdout = format!("{:#}\n", serde_json::Value::from(hits.to_vec())).into_bytes();
    }

    pub(crate) fn settings(&self) -> Result<TemperSettings, Box<dyn Error>> {
        let environment = GateEnvironment::from_vars([
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("GITHUB_TOKEN"), OsString::from("secret")),
        ]);
        Ok(TemperSettings::new(
            &self.pinned,
            &self.head_tree,
            &environment,
            "run-1",
        )?)
    }

    /// Runs the temper plugin in `job`.
    pub(crate) fn run(
        &mut self,
        job: GateJob,
    ) -> Result<Result<GateRun, TemperError<NeverFails, NeverFails>>, Box<dyn Error>> {
        let config = config()?;
        let temper = plugin(&config, "temper")?;
        let settings = self.settings()?;
        let mut adapter = TemperAdapter::new(&mut self.scan, &self.revisions, settings);
        Ok(adapter.run(&temper, job))
    }

    /// Runs `gates-static` through the gate use case, and returns its outcome with the rule,
    /// file, line and severity of each finding in the validated document.
    pub(crate) fn outcome(&mut self) -> Result<(GateOutcome, Vec<Seen>), Box<dyn Error>> {
        let config = config()?;
        let settings = self.settings()?;
        let mut adapter = TemperAdapter::new(&mut self.scan, &self.revisions, settings);
        let validator = ContractValidator::new()?;
        let reports = run_gates(&config, GateJob::Static, &mut adapter, &validator);
        let [report] = reports.as_slice() else {
            return Err(format!("expected one report, got {reports:?}").into());
        };
        let seen = match &report.document {
            None => Vec::new(),
            Some(document) => {
                let document: FindingsDocument = serde_json::from_value(document.clone())?;
                document
                    .findings
                    .into_iter()
                    .map(|finding| {
                        let rule = finding.rule.unwrap_or_default();
                        (rule, finding.file, finding.line.get(), finding.severity)
                    })
                    .collect()
            }
        };
        Ok((report.outcome, seen))
    }
}

fn write(path: &Path, text: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}
