//! Fakes and builders for the eval harness: a scripted model runner, an in-memory eval ledger
//! and corpus entries for one split.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use assay_app::{
    CalendarDate, ClassMap, CorpusEntry, CorpusEntryKind, EvalCorpus, EvalLedger, EvalMonth,
    EvalRequest, ModelCall, ModelOutcome, ModelResult, ModelRunner, PromptBundle, Split,
    Structured, read_eval_corpus,
};
use assay_domain::Aeu;
use serde_json::{Value, json};

use crate::check_fakes::clean_init;

/// What every scripted call costs unless a test says otherwise: 0.10 AEU.
pub(crate) const CALL_COST: Aeu = Aeu::from_micros(100_000);

/// The split directory every request names.
pub(crate) const CORPUS_DIR: &str = "/corpus/dev";

/// The labelled hunk of every defect case: `src/lib.rs` lines 40 to 44, a logic defect.
pub(crate) const DEFECT_JSON: &str = r#"{
  "schema": "assay.eval-case/1",
  "authoring_model": "human",
  "source": "real-defect",
  "labels": [{"file": "src/lib.rs", "start_line": 40, "end_line": 44, "class": "logic"}]
}"#;

/// A true-negative case.
pub(crate) const QUIET_JSON: &str = r#"{
  "schema": "assay.eval-case/1",
  "authoring_model": "claude-opus-5-5",
  "source": "true-negative",
  "labels": []
}"#;

/// The diff every built case carries.
pub(crate) const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\n+fn changed() {}\n";

/// The runner could not make the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unreachable;

impl fmt::Display for Unreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the model could not be reached")
    }
}

impl Error for Unreachable {}

/// One scripted answer.
#[derive(Debug, Clone)]
pub(crate) enum Answer {
    /// The call ran and came to this.
    Ran(ModelResult),
    /// The runner failed.
    Fails,
}

/// A runner that answers calls in order from a script, then with no finding, and records
/// every call.
#[derive(Debug, Default)]
pub(crate) struct ScriptedModel {
    answers: Mutex<VecDeque<Answer>>,
    calls: Mutex<Vec<ModelCall>>,
}

impl ScriptedModel {
    pub(crate) fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            calls: Mutex::default(),
        }
    }

    pub(crate) fn calls(&self) -> Vec<ModelCall> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ModelRunner for ScriptedModel {
    type Error = Unreachable;

    fn run(&self, call: &ModelCall) -> Result<ModelResult, Unreachable> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call.clone());
        let answer = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        match answer {
            Some(Answer::Ran(result)) => Ok(result),
            Some(Answer::Fails) => Err(Unreachable),
            None => Ok(ran(output(&[]))),
        }
    }
}

/// A clean call that produced `output` at [`CALL_COST`].
pub(crate) fn ran(output: Value) -> ModelResult {
    ended(ModelOutcome::Ok(Structured::new(output)))
}

/// A clean call that ended with `outcome` at [`CALL_COST`].
pub(crate) fn ended(outcome: ModelOutcome) -> ModelResult {
    ModelResult {
        outcome,
        cost: CALL_COST,
        rate_limits: Vec::new(),
        init: clean_init(),
    }
}

/// A findings document holding `findings`.
pub(crate) fn output(findings: &[Value]) -> Value {
    json!({ "findings": findings })
}

/// A finding at `file:line` of `class`.
pub(crate) fn finding(file: &str, line: u64, class: &str) -> Value {
    json!({"file": file, "line": line, "class": class, "title": "t", "severity": "major"})
}

/// An answer that detects the defect of [`DEFECT_JSON`].
pub(crate) fn hit() -> Answer {
    Answer::Ran(ran(output(&[finding("src/lib.rs", 42, "logic")])))
}

/// An answer that reports nothing.
pub(crate) fn quiet() -> Answer {
    Answer::Ran(ran(output(&[])))
}

/// An answer that reports one finding far from any label.
pub(crate) fn stray() -> Answer {
    Answer::Ran(ran(output(&[finding("src/other.rs", 1, "logic")])))
}

/// The eval ledger could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LedgerDown;

impl fmt::Display for LedgerDown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the eval ledger is down")
    }
}

impl Error for LedgerDown {}

/// An in-memory eval ledger: one month's opening spend and every charge made.
#[derive(Debug, Default)]
pub(crate) struct FakeLedger {
    pub(crate) opening: Aeu,
    pub(crate) charges: Vec<(EvalMonth, Aeu)>,
    pub(crate) unreadable: bool,
    pub(crate) charges_fail: bool,
}

impl FakeLedger {
    pub(crate) fn with_spent(opening: Aeu) -> Self {
        Self {
            opening,
            ..Self::default()
        }
    }
}

impl EvalLedger for FakeLedger {
    type Error = LedgerDown;

    fn spent(&self, month: EvalMonth) -> Result<Aeu, LedgerDown> {
        if self.unreadable {
            return Err(LedgerDown);
        }
        let charged = self
            .charges
            .iter()
            .filter(|(charged_in, _)| *charged_in == month)
            .fold(Aeu::ZERO, |sum, (_, cost)| sum.saturating_add(*cost));
        Ok(self.opening.saturating_add(charged))
    }

    fn charge(&mut self, month: EvalMonth, cost: Aeu) -> Result<(), LedgerDown> {
        if self.charges_fail {
            return Err(LedgerDown);
        }
        self.charges.push((month, cost));
        Ok(())
    }
}

/// The entries of a complete case `id`: its directory, `case.json`, `change.diff` and a tree
/// with one file.
pub(crate) fn case_entries(id: &str, json: &str, diff: &str) -> Vec<CorpusEntry> {
    vec![
        dir(id),
        file(&format!("{id}/case.json"), json.as_bytes()),
        file(&format!("{id}/change.diff"), diff.as_bytes()),
        dir(&format!("{id}/tree")),
        dir(&format!("{id}/tree/src")),
        file(&format!("{id}/tree/src/lib.rs"), b""),
    ]
}

pub(crate) fn dir(path: &str) -> CorpusEntry {
    CorpusEntry {
        path: path.to_owned(),
        kind: CorpusEntryKind::Directory,
    }
}

pub(crate) fn file(path: &str, bytes: &[u8]) -> CorpusEntry {
    CorpusEntry {
        path: path.to_owned(),
        kind: CorpusEntryKind::File(bytes.to_vec()),
    }
}

/// A dev split of the given `(id, case.json)` cases.
pub(crate) fn corpus(cases: &[(&str, &str)]) -> Result<EvalCorpus, Box<dyn Error>> {
    let entries: Vec<CorpusEntry> = cases
        .iter()
        .flat_map(|(id, json)| case_entries(id, json, DIFF))
        .collect();
    Ok(read_eval_corpus(Split::Dev, &entries)?)
}

/// 2026-10-13, so charges go to 2026-10.
pub(crate) fn today() -> Result<CalendarDate, Box<dyn Error>> {
    Ok("2026-10-13".parse()?)
}

/// A request over `corpus` with the stand-in bundle and the identity class map.
pub(crate) fn request<'a>(
    corpus: &'a EvalCorpus,
    bundle: &'a PromptBundle,
    map: &'a ClassMap,
    today: CalendarDate,
) -> EvalRequest<'a> {
    EvalRequest {
        corpus,
        corpus_dir: Path::new(CORPUS_DIR),
        bundle,
        system_prompt_file: Path::new("/bundle/system.md"),
        class_map: map,
        timeout: Duration::from_secs(600),
        today,
    }
}
