//! A small change and reviewer documents to validate against it.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use assay_app::{
    Change, ContractSchemaError, ContractValidator, DiffPaths, FileDiff, Hunk, Inputs, LineSpan,
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

/// A reviewer document holding `findings`.
pub(crate) fn document(findings: &[Value]) -> Vec<u8> {
    json!({
        "schema": "assay.findings/1",
        "source": {
            "kind": "reviewer",
            "name": "claude",
            "version": "git:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "role": "combined",
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
}

impl Default for Run<'_> {
    fn default() -> Self {
        Self {
            change: change(),
            suppress: &NoRules,
            dismissals: Vec::new(),
            gate_findings: Vec::new(),
        }
    }
}

/// One validator for every test: compiling the schemas dominates a property test's run time.
static VALIDATOR: LazyLock<Result<ContractValidator, ContractSchemaError>> =
    LazyLock::new(ContractValidator::new);

impl Run<'_> {
    /// Validates `documents` in this run.
    pub(crate) fn validate(
        &self,
        documents: &[Vec<u8>],
    ) -> Result<Validated, &'static ContractSchemaError> {
        let validator = VALIDATOR.as_ref()?;
        let inputs = Inputs {
            repository: REPOSITORY,
            change: &self.change,
            suppress: self.suppress,
            dismissals: &self.dismissals,
            gate_findings: &self.gate_findings,
        };
        let documents: Vec<&[u8]> = documents.iter().map(Vec::as_slice).collect();
        Ok(validate(validator, &inputs, &documents))
    }
}

/// Validates `findings` in one document against the default run.
pub(crate) fn validate_one(findings: &[Value]) -> Result<Validated, &'static ContractSchemaError> {
    Run::default().validate(&[document(findings)])
}
