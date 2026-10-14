//! A small change and reviewer documents to validate against it.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use assay_app::{
    Change, CommitId, ContractSchemaError, ContractValidator, DiffPaths, EgressScan, EgressToken,
    FileDiff, Hunk, Inputs, KnownRuns, KnownRunsError, LineSpan, ReviewerDocument, Role,
    SuppressRules, Validated, validate,
};
use assay_domain::{Dismissal, Finding};
use serde_json::{Value, json};

/// The file every default finding cites.
pub(crate) const LIB: &str = "src/lib.rs";

/// `src/lib.rs` at head. The hunk covers lines 1 to 5; `other` (lines 7 to 10) is unchanged.
pub(crate) const LIB_AT_HEAD: &str = "\
pub fn parse(input: &str) -> Result<u32, Error> {
    let x = input.parse().unwrap();
    Ok(x)
}

pub fn other(input: &str) -> u32 {
    let x = input.parse().unwrap();
    x
}
";

/// `src/lib.rs` at base.
pub(crate) const LIB_AT_BASE: &str = "\
pub fn parse(input: &str) -> Result<u32, Error> {
    Ok(input.len())
}

pub fn other(input: &str) -> u32 {
    let x = input.parse().unwrap();
    x
}
";

/// The repository the change belongs to.
pub(crate) const REPOSITORY: &str = "Wet-Ink-Corporation/assay";

/// A change that modifies `src/lib.rs` in one hunk: base lines 1 to 3 became head lines 1 to 5.
pub(crate) fn change() -> Change {
    Change {
        head_files: BTreeMap::from([(LIB.to_owned(), LIB_AT_HEAD.to_owned())]),
        base_files: BTreeMap::from([(LIB.to_owned(), LIB_AT_BASE.to_owned())]),
        diffs: vec![FileDiff {
            paths: DiffPaths::Modified {
                path: LIB.to_owned(),
            },
            hunks: vec![Hunk {
                base: LineSpan { start: 1, count: 3 },
                head: LineSpan { start: 1, count: 5 },
            }],
        }],
    }
}

/// A valid `major` defect on line 2 of `src/lib.rs`, with a trigger.
pub(crate) fn finding() -> Value {
    json!({
        "class": "error-handling",
        "kind": "defect",
        "severity": "major",
        "introduced_by": "diff",
        "file": LIB,
        "line": 2,
        "side": "RIGHT",
        "evidence": "let x = input.parse().unwrap();",
        "trigger": "a non-numeric input reaches line 2 through parse",
        "title": "unwrap on user input panics",
        "body": "`parse` fails on non-numeric input, so the handler panics.",
    })
}

/// `finding()` with `changes` applied on top (a `null` removes the field).
pub(crate) fn finding_with(changes: Value) -> Value {
    let mut finding = finding();
    if let (Some(fields), Value::Object(changes)) = (finding.as_object_mut(), changes) {
        for (key, value) in changes {
            if value.is_null() {
                fields.remove(&key);
            } else {
                fields.insert(key, value);
            }
        }
    }
    finding
}

/// A `combined` reviewer document holding `findings`.
pub(crate) fn document(findings: &[Value]) -> Vec<u8> {
    document_by("combined", findings)
}

/// A reviewer document from `role` holding `findings`.
pub(crate) fn document_by(role: &str, findings: &[Value]) -> Vec<u8> {
    json!({
        "schema": "assay.findings/1",
        "source": {
            "kind": "reviewer",
            "name": "claude",
            "version": "git:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "role": role,
            "run_key": "run-42-1",
        },
        "findings": findings,
    })
    .to_string()
    .into_bytes()
}

/// Suppresses nothing.
#[derive(Debug)]
pub(crate) struct NoRules;

impl SuppressRules for NoRules {
    fn suppresses(&self, _finding: &Finding, _file_at_head: &str) -> bool {
        false
    }
}

/// What a run validates with, besides the documents.
pub(crate) struct Run<'a> {
    pub(crate) change: Change,
    pub(crate) suppress: &'a dyn SuppressRules,
    pub(crate) dismissals: Vec<Dismissal<'a>>,
    pub(crate) gate_findings: Vec<Finding>,
    /// The OAuth token, as Z3 holds it; Z4 holds none (05 §4).
    pub(crate) token: Option<EgressToken>,
    /// Base `REVIEW.md`, whose long runs explain a finding's as the tree's do (03 §3).
    pub(crate) review_md: String,
    /// The run's base and head commits, which explain themselves.
    pub(crate) commits: Vec<CommitId>,
}

impl Default for Run<'_> {
    fn default() -> Self {
        Self {
            change: change(),
            suppress: &NoRules,
            dismissals: Vec::new(),
            gate_findings: Vec::new(),
            token: None,
            review_md: String::new(),
            commits: Vec::new(),
        }
    }
}

/// Why a run could not validate: a fixture defect, never an outcome under test.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RunError {
    /// The embedded schemas do not compile.
    #[error(transparent)]
    Schema(#[from] &'static ContractSchemaError),
    /// The run's tree is past the egress index cap.
    #[error(transparent)]
    Index(#[from] KnownRunsError),
}

/// One validator for every test: compiling the schemas dominates a property test's run time.
static VALIDATOR: LazyLock<Result<ContractValidator, ContractSchemaError>> =
    LazyLock::new(ContractValidator::new);

impl Run<'_> {
    /// Validates `documents` in this run, each as the `combined` reviewer's.
    pub(crate) fn validate(&self, documents: &[Vec<u8>]) -> Result<Validated, RunError> {
        self.validate_as(Role::Combined, documents)
    }

    /// Validates `documents` in this run, each as the output of a call that ran as `role`.
    pub(crate) fn validate_as(
        &self,
        role: Role,
        documents: &[Vec<u8>],
    ) -> Result<Validated, RunError> {
        let validator = VALIDATOR.as_ref()?;
        // The tree is every file of the change at either end, and base REVIEW.md.
        let tree = self
            .change
            .head_files
            .values()
            .chain(self.change.base_files.values())
            .chain([&self.review_md])
            .map(String::as_bytes);
        let known = KnownRuns::index(tree)?.with_commits(&self.commits);
        let egress = match &self.token {
            Some(token) => EgressScan::with_token(token, known),
            None => EgressScan::without_token(known),
        };
        let inputs = Inputs {
            repository: REPOSITORY,
            change: &self.change,
            suppress: self.suppress,
            dismissals: &self.dismissals,
            gate_findings: &self.gate_findings,
            egress: &egress,
        };
        let documents: Vec<ReviewerDocument<'_>> = documents
            .iter()
            .map(|bytes| ReviewerDocument { role, bytes })
            .collect();
        Ok(validate(validator, &inputs, &documents))
    }
}

/// Validates `findings` in one document against the default run.
pub(crate) fn validate_one(findings: &[Value]) -> Result<Validated, RunError> {
    Run::default().validate(&[document(findings)])
}
