//! Fakes and fixtures for the temper adapter: PR files held in memory, a scripted `temper scan`
//! and a pinned temper copy on disk.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use assay_app::{
    ContractValidator, GateJob, GateOutcome, GateRun, GateRunner, Termination, run_gates,
};
use assay_domain::{FindingsDocument, Severity};
use assay_gates::{
    CaptureSupervisor, CapturedRun, EntryKind, GateEnvironment, ProcessSpec, Revision,
    RevisionFiles, TemperAdapter, TemperError, TemperPaths, TemperSettings, TreeEntry,
};
use tempfile::TempDir;

use crate::support::{SeenSpec, config, plugin};

pub(crate) const BASELINE: &str = ".claude/temper-baseline.json";
pub(crate) const CONFIG: &str = ".claude/temper.json";
pub(crate) const EXCEPTIONS: &str = ".assay/exceptions.toml";
pub(crate) const NODE: &str = "/opt/node/bin/node";

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

/// A baseline with `counts` as `(crate, lint, count)` and `crates` as `(name, dir)`, laid out
/// the way temper writes it.
pub(crate) fn baseline_with(counts: &[(&str, &str, u64)], crates: &[(&str, &str)]) -> String {
    let mut totals: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
    for (krate, lint, count) in counts {
        totals.entry(krate).or_default().insert(lint, *count);
    }
    let crates: BTreeMap<&str, &str> = crates.iter().copied().collect();
    let document = serde_json::json!({
        "temper_baseline": 1,
        "created": "2026-10-01T00:00:00.000Z",
        "updated": "2026-10-01T00:00:00.000Z",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "toolchain": "1.98.1",
        "host": "x86_64-unknown-linux-gnu",
        "crates": crates,
        "totals": totals,
    });
    format!("{document:#}\n")
}

/// The crates every fixture baseline names.
pub(crate) const CRATES: [(&str, &str); 2] = [
    ("assay-forge", "crates/assay-forge"),
    ("assay-gates", "crates/assay-gates"),
];

/// A baseline whose totals are `counts`, with [`CRATES`].
pub(crate) fn baseline(counts: &[(&str, &str, u64)]) -> String {
    baseline_with(counts, &CRATES)
}

/// One `temper scan --all --json` hit, shaped like temper 1.3.0's `scanSource` output with
/// the `file` the CLI adds, at `(file, line)`; `key` is `expect-attr` for the attribute's own
/// hit.
pub(crate) fn hit(
    key: &str,
    (file, line): (&str, u64),
    severity: &str,
    exception: Option<&str>,
) -> serde_json::Value {
    let mut hit = serde_json::json!({
        "key": key,
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

/// A fake object id: deterministic within the test process, equal for equal bytes.
pub(crate) fn object_id(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:040x}", u128::from(hasher.finish()))
}

/// The PR's base and head files, in memory.
#[derive(Debug, Default)]
pub(crate) struct FakeRevisions {
    files: BTreeMap<(Revision, String), (EntryKind, Vec<u8>)>,
    /// When set, listing this path fails.
    pub(crate) failing: Option<String>,
}

/// Why the fake would not answer.
#[derive(Debug)]
pub(crate) struct FakeError;

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("scripted read failure")
    }
}

impl Error for FakeError {}

impl RevisionFiles for FakeRevisions {
    type Error = FakeError;

    fn list(&self, revision: Revision, path: Option<&str>) -> Result<Vec<TreeEntry>, FakeError> {
        if path.is_some() && self.failing.as_deref() == path {
            return Err(FakeError);
        }
        let within = |name: &str| match path {
            None => true,
            Some(path) => {
                name == path
                    || name
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/'))
            }
        };
        Ok(self
            .files
            .iter()
            .filter(|((side, name), _)| *side == revision && within(name))
            .map(|((_, name), (kind, bytes))| TreeEntry {
                path: name.clone(),
                kind: *kind,
                object: object_id(bytes),
                size: u64::try_from(bytes.len()).ok(),
            })
            .collect())
    }

    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, FakeError> {
        entries
            .iter()
            .map(|entry| {
                self.files
                    .values()
                    .find(|(_, bytes)| object_id(bytes) == entry.object)
                    .map(|(_, bytes)| bytes.clone())
                    .ok_or(FakeError)
            })
            .collect()
    }

    fn blob_id(&self, bytes: &[u8]) -> Result<String, FakeError> {
        Ok(object_id(bytes))
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
        Ok(CapturedRun {
            termination: self.termination,
            stdout: self.stdout.clone(),
        })
    }
}

/// The adapter's result with the fakes.
pub(crate) type Ran = Result<GateRun, TemperError<FakeError, FakeError>>;

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

    /// Sets `path` to the regular file `text` on `revision`.
    pub(crate) fn set(&mut self, revision: Revision, path: &str, text: &str) {
        self.entry(revision, path, (EntryKind::File, text));
    }

    /// Sets `path` on `revision` to a symlink to `target`.
    pub(crate) fn link(&mut self, revision: Revision, path: &str, target: &str) {
        self.entry(revision, path, (EntryKind::Symlink, target));
    }

    /// Sets `path` on `revision` to a submodule.
    pub(crate) fn submodule(&mut self, revision: Revision, path: &str) {
        self.entry(revision, path, (EntryKind::Submodule, ""));
    }

    /// Sets `path` on `revision` to an entry of a kind whose blob is the text.
    fn entry(&mut self, revision: Revision, path: &str, (kind, text): (EntryKind, &str)) {
        let content = (kind, text.as_bytes().to_vec());
        self.revisions
            .files
            .insert((revision, path.to_owned()), content);
    }

    /// Sets `path` to `text` on both sides.
    pub(crate) fn both(&mut self, path: &str, text: &str) {
        self.set(Revision::Base, path, text);
        self.set(Revision::Head, path, text);
    }

    pub(crate) fn remove(&mut self, revision: Revision, path: &str) {
        self.revisions.files.remove(&(revision, path.to_owned()));
    }

    /// Makes the scan print `hits` as a JSON array.
    pub(crate) fn scan_prints(&mut self, hits: &[serde_json::Value]) {
        self.scan.stdout = format!("{:#}\n", serde_json::Value::from(hits.to_vec())).into_bytes();
    }

    pub(crate) fn paths(&self) -> TemperPaths {
        TemperPaths {
            node: PathBuf::from(NODE),
            pinned: self.pinned.clone(),
            head_tree: self.head_tree.clone(),
        }
    }

    pub(crate) fn settings(&self) -> Result<TemperSettings, Box<dyn Error>> {
        let environment = GateEnvironment::from_vars([
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("GITHUB_TOKEN"), OsString::from("secret")),
        ]);
        Ok(TemperSettings::new(self.paths(), &environment, "run-1")?)
    }

    /// Runs the temper plugin in `job`.
    pub(crate) fn run(&mut self, job: GateJob) -> Result<Ran, Box<dyn Error>> {
        let config = config()?;
        let temper = plugin(&config, "temper")?;
        let settings = self.settings()?;
        let mut adapter = TemperAdapter::new(&mut self.scan, &self.revisions, settings);
        Ok(adapter.run(&temper, job))
    }

    /// Runs `gates-static` through the gate use case, and returns its outcome with the rule,
    /// file, line and severity of each finding in the validated document.
    pub(crate) fn outcome(&mut self) -> Result<(GateOutcome, Vec<Seen>), Box<dyn Error>> {
        let document = self.document()?;
        let seen = document
            .1
            .map(|document| {
                document
                    .findings
                    .into_iter()
                    .map(|finding| {
                        let rule = finding.rule.unwrap_or_default();
                        (rule, finding.file, finding.line.get(), finding.severity)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((document.0, seen))
    }

    /// Runs `gates-static` through the gate use case: the outcome and the validated document.
    pub(crate) fn document(
        &mut self,
    ) -> Result<(GateOutcome, Option<FindingsDocument>), Box<dyn Error>> {
        let config = config()?;
        let settings = self.settings()?;
        let mut adapter = TemperAdapter::new(&mut self.scan, &self.revisions, settings);
        let validator = ContractValidator::new()?;
        let reports = run_gates(&config, GateJob::Static, &mut adapter, &validator);
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
}

fn write(path: &Path, text: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}
