//! In-memory fakes for `check_local` (TST-09): the change's patches, the local gate runner, a
//! scripted model and an objects port that fails on one file. The PR itself is
//! `plan_fakes::FakePr`.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{
    BundleFiles, ChangePatches, ChangedFiles, CheckRequest, CommitId, FilePatch, GateDocument,
    GateJob, GateRun, GateRunner, Hunk, InitParts, InitReadError, LineSpan, ModelCall,
    ModelOutcome, ModelResult, ModelRunner, PatchBudget, PlanRequest, Plugin, PromptBundle,
    PullRequestObjects, ReviewSetup, SessionInit, Structured, Termination, TreeEntry,
};
use assay_domain::{Aeu, FileChange, Lines};
use serde_json::{Value, json};

use crate::plan_fakes::{BASE, DIGEST, FakeError, FakePr, HEAD, request};

/// `src/lib.rs` at base.
pub(crate) const LIB_AT_BASE: &str =
    "pub fn parse(input: &str) -> u32 {\n    input.parse().unwrap_or(0)\n}\n";

/// `src/lib.rs` at head: the fallback became an `unwrap`.
pub(crate) const LIB_AT_HEAD: &str =
    "pub fn parse(input: &str) -> u32 {\n    input.parse().unwrap()\n}\n";

/// The tree fingerprint the requests name.
pub(crate) const TREE: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// A base config with one required `exec` gate, `unit`, in the test job, and `main` protected.
pub(crate) fn unit_gate_config() -> String {
    format!(
        r#"
[assay]
digest = "{DIGEST}"
protected_bases = ["main"]

[[plugin]]
name = "unit"
adapter = "exec"
exec = ["./scripts/test.sh"]
job = "test"
required = true
"#
    )
}

/// The fake PR with [`unit_gate_config`] and the `unwrap` edit to `src/lib.rs`.
pub(crate) fn dirty_tree() -> FakePr {
    let mut pr = FakePr::new();
    for commit in [BASE, HEAD] {
        pr.write(commit, ".assay/plugins.toml", unit_gate_config().as_bytes());
    }
    pr.write(BASE, "src/lib.rs", LIB_AT_BASE.as_bytes());
    pr.write(HEAD, "src/lib.rs", LIB_AT_HEAD.as_bytes());
    pr.changes = ChangedFiles::Listed(vec![FileChange::new(
        "src/lib.rs",
        Lines::Counted {
            added: 1,
            removed: 1,
        },
    )]);
    pr
}

/// The bundle every check uses; its role instructions are recognisable in a prompt.
pub(crate) fn bundle() -> Result<PromptBundle, Box<dyn Error>> {
    Ok(PromptBundle::new(BundleFiles {
        system: "SYSTEM".to_owned(),
        combined: "INSTRUCTION combined".to_owned(),
        combined_micro: "INSTRUCTION combined-micro".to_owned(),
        verifier: "INSTRUCTION verifier".to_owned(),
    })?)
}

/// What a check is asked, borrowed by [`CheckRequest`].
pub(crate) struct Setup {
    pub(crate) plan: PlanRequest,
    pub(crate) bundle: PromptBundle,
    pub(crate) rubric: Vec<u8>,
    pub(crate) system: PathBuf,
    pub(crate) tree_dir: PathBuf,
}

impl Setup {
    pub(crate) fn of(pr: &FakePr) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            plan: request(pr)?,
            bundle: bundle()?,
            rubric: rubric_bytes()?,
            system: PathBuf::from("/bundle/v1/system.md"),
            tree_dir: PathBuf::from("/scratch/tree"),
        })
    }

    pub(crate) fn request(&self) -> CheckRequest<'_> {
        CheckRequest {
            plan: &self.plan,
            tree: TREE,
            rubric: &self.rubric,
            review: ReviewSetup {
                bundle: &self.bundle,
                system_prompt_file: &self.system,
                tree_dir: &self.tree_dir,
                timeout: Duration::from_mins(15),
            },
        }
    }
}

/// `rubric/v1.toml` as bytes.
pub(crate) fn rubric_bytes() -> Result<Vec<u8>, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rubric/v1.toml");
    Ok(std::fs::read(path)?)
}

/// Patches by head path; a file without one has an empty patch.
#[derive(Debug, Default)]
pub(crate) struct FakePatches {
    pub(crate) by_path: BTreeMap<String, FilePatch>,
    /// Every call fails.
    pub(crate) failing: bool,
    /// One patch fewer than files asked for.
    pub(crate) short: bool,
    /// The budget of each call, in order.
    pub(crate) budgets: RefCell<Vec<PatchBudget>>,
}

impl FakePatches {
    /// The `unwrap` edit's patch: one hunk, base and head lines 1 to 3.
    pub(crate) fn lib() -> Self {
        let mut patches = Self::default();
        patches.by_path.insert(
            "src/lib.rs".to_owned(),
            FilePatch {
                text: Some(
                    "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1,3 +1,3 @@\n \
                     pub fn parse(input: &str) -> u32 {\n-    input.parse().unwrap_or(0)\n\
                     +    input.parse().unwrap()\n }\n"
                        .to_owned(),
                ),
                hunks: vec![Hunk {
                    base: LineSpan { start: 1, count: 3 },
                    head: LineSpan { start: 1, count: 3 },
                }],
            },
        );
        patches
    }
}

impl ChangePatches for FakePatches {
    type Error = FakeError;

    fn patches(
        &self,
        (base, head): (&CommitId, &CommitId),
        files: &[FileChange],
        mut budget: PatchBudget,
    ) -> Result<Vec<FilePatch>, FakeError> {
        self.budgets.borrow_mut().push(budget);
        if self.failing || (base.as_str(), head.as_str()) != (BASE, HEAD) {
            return Err(FakeError);
        }
        let mut patches: Vec<FilePatch> = files
            .iter()
            .map(|file| {
                let patch = self.by_path.get(&file.path).cloned().unwrap_or_default();
                FilePatch {
                    text: patch.text.and_then(|text| budget.keep(text)),
                    hunks: patch.hunks,
                }
            })
            .collect();
        if self.short {
            patches.pop();
        }
        Ok(patches)
    }
}

/// A gate the local runner does not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotLocal;

impl fmt::Display for NotLocal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not run in local mode")
    }
}

impl Error for NotLocal {}

/// Runs scripted gates by plugin name; any other plugin is not run locally.
#[derive(Debug, Default)]
pub(crate) struct FakeGates {
    runs: BTreeMap<String, GateRun>,
    pub(crate) calls: Vec<(String, GateJob)>,
}

impl FakeGates {
    /// `gate` exits 0 with a document concluding `conclusion` and holding `findings`.
    pub(crate) fn concluding(gate: &str, conclusion: &str, findings: &Value) -> Self {
        let mut gates = Self::default();
        let document = json!({
            "schema": "assay.findings/1",
            "source": {"kind": "gate", "name": gate, "version": "local", "run_key": "local"},
            "conclusion": conclusion,
            "findings": findings,
        });
        let exit = i32::from(conclusion == "fail");
        gates.runs.insert(
            gate.to_owned(),
            GateRun {
                termination: Termination::Exited(exit),
                document: GateDocument::Written(document.to_string().into_bytes()),
            },
        );
        gates
    }

    /// `unit` passes with no finding.
    pub(crate) fn passing() -> Self {
        Self::concluding("unit", "pass", &json!([]))
    }
}

impl GateRunner for FakeGates {
    type Error = NotLocal;

    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, NotLocal> {
        self.calls.push((plugin.name().as_str().to_owned(), job));
        self.runs.remove(plugin.name().as_str()).ok_or(NotLocal)
    }
}

/// The scripted model could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NoCli;

impl fmt::Display for NoCli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("claude could not be started")
    }
}

impl Error for NoCli {}

/// Answers each call with the next scripted answer, recording the calls. A `RefCell` because
/// the port takes `&self`; the fake is used by one test thread only.
#[derive(Debug, Default)]
pub(crate) struct FakeModel {
    pub(crate) answers: RefCell<VecDeque<Result<ModelOutcome, NoCli>>>,
    pub(crate) calls: RefCell<Vec<ModelCall>>,
    /// The session's tools, when not the clean three; `Some(&[])` is an unreadable init.
    pub(crate) tools: Option<&'static [&'static str]>,
}

impl FakeModel {
    pub(crate) fn answering(
        answers: impl IntoIterator<Item = Result<ModelOutcome, NoCli>>,
    ) -> Self {
        Self {
            answers: RefCell::new(answers.into_iter().collect()),
            calls: RefCell::default(),
            tools: None,
        }
    }

    /// One structured answer holding `findings`.
    pub(crate) fn finding(findings: &[Value]) -> Self {
        Self::answering([Ok(ModelOutcome::Ok(Structured::new(
            json!({"findings": findings}),
        )))])
    }

    pub(crate) fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

/// A session confined to `Read`, `Grep` and `Glob` with no MCP server.
pub(crate) fn clean_init() -> Result<SessionInit, InitReadError> {
    session(&["Read", "Grep", "Glob"])
}

/// A session with `tools` and nothing else loaded.
pub(crate) fn session(tools: &[&str]) -> Result<SessionInit, InitReadError> {
    SessionInit::new(InitParts {
        cli_version: "2.1.0".to_owned(),
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        mcp_servers: Vec::new(),
        plugins: Vec::new(),
        skills: Vec::new(),
        agents: Vec::new(),
    })
}

impl ModelRunner for FakeModel {
    type Error = NoCli;

    fn run(&self, call: &ModelCall) -> Result<ModelResult, NoCli> {
        // The fake keeps each call so tests can read what was asked.
        self.calls.borrow_mut().push(call.clone());
        let outcome = self
            .answers
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(NoCli))?;
        Ok(ModelResult {
            outcome,
            cost: Aeu::from_micros(120_000),
            rate_limits: Vec::new(),
            init: match self.tools {
                None => clean_init(),
                Some([]) => Err(InitReadError::Missing),
                Some(tools) => session(tools),
            },
        })
    }
}

/// The `unwrap` finding on line 2 of `src/lib.rs`, as the model writes it.
pub(crate) fn unwrap_finding() -> Value {
    json!({
        "class": "error-handling",
        "kind": "defect",
        "severity": "major",
        "introduced_by": "diff",
        "file": "src/lib.rs",
        "line": 2,
        "side": "RIGHT",
        "evidence": "input.parse().unwrap()",
        "trigger": "a non-numeric input reaches parse",
        "title": "unwrap on user input panics",
        "body": "parse fails on non-numeric input, so the caller panics.",
    })
}

/// `pr`'s objects, except that reading `path` fails, or comes back one blob short.
#[derive(Debug)]
pub(crate) struct Unreadable<'a> {
    pub(crate) pr: &'a FakePr,
    pub(crate) path: &'static str,
    pub(crate) short: bool,
}

impl PullRequestObjects for Unreadable<'_> {
    type Error = FakeError;

    fn changed_files(&self, base: &CommitId, head: &CommitId) -> Result<ChangedFiles, FakeError> {
        self.pr.changed_files(base, head)
    }

    fn list(&self, commit: &CommitId, path: Option<&str>) -> Result<Vec<TreeEntry>, FakeError> {
        self.pr.list(commit, path)
    }

    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, FakeError> {
        if !entries.iter().any(|entry| entry.path == self.path) {
            return self.pr.read(entries);
        }
        if !self.short {
            return Err(FakeError);
        }
        let mut blobs = self.pr.read(entries)?;
        blobs.pop();
        Ok(blobs)
    }
}
