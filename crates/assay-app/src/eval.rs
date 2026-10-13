//! The corpus eval harness (`docs/architecture/04-pipeline.md` §8, `06-operations.md` F11).
//!
//! [`read_eval_corpus`] reads one split of `corpus/{dev,test,inject}` as the caller listed it;
//! [`run_eval`] runs each case [`EVAL_RUNS`] times through a [`ModelRunner`], scores every run
//! deterministically and charges the separate eval ledger; [`compare`] applies the accept rule
//! to a candidate report against a baseline. Nothing here reads files or the production cap.

mod case;
mod corpus;
mod ledger;
mod report;
mod score;

use std::fmt;
use std::path::Path;
use std::time::Duration;

use assay_domain::Aeu;
use thiserror::Error;

use crate::calendar::CalendarDate;
use crate::contract::Contract;
use crate::model_runner::{
    InvalidPrompt, ModelCall, ModelOutcome, ModelResult, ModelRunner, Prompt, RetryError,
    run_with_retry,
};
use crate::prompt_bundle::{PromptBundle, Role};
use crate::session_init::SessionInit;

pub use case::{
    AuthoringModel, CASE_SCHEMA, CaseProblem, CaseSource, Label, MAX_AUTHORING_MODEL_BYTES,
    MAX_LABEL_PATH_BYTES, MAX_LABELS,
};
pub use corpus::{
    CorpusError, EvalCase, EvalCorpus, MAX_CASE_BYTES, MAX_CASE_ID_BYTES, MAX_EVAL_CASES,
    MAX_EVAL_ENTRIES, Split, read_eval_corpus,
};
pub use ledger::{EVAL_MONTHLY_CAP, EvalLedger, EvalMonth};
pub use report::{
    Acceptance, CaseKind, CaseOutcome, CompareError, EvalReport, NoiseCheck, Proportion, RunScore,
    Unscorable, WILSON_Z, compare,
};
pub use score::{ClassMap, LINE_WINDOW, MAX_RUN_FINDINGS, OutputProblem};

/// Runs per case (04 §8: k = 3).
pub const EVAL_RUNS: usize = 3;

/// The role every eval call reviews as: the `combined` reviewer.
pub const EVAL_ROLE: Role = Role::Combined;

/// What one evaluation runs.
#[derive(Debug, Clone, Copy)]
pub struct EvalRequest<'a> {
    /// The split's cases.
    pub corpus: &'a EvalCorpus,
    /// The split's directory (absolute); a case's model reads only `<corpus_dir>/<id>/tree`.
    pub corpus_dir: &'a Path,
    /// The prompt bundle under evaluation.
    pub bundle: &'a PromptBundle,
    /// The bundle's system prompt file (absolute).
    pub system_prompt_file: &'a Path,
    /// Which finding classes count for which label class.
    pub class_map: &'a ClassMap,
    /// The wall-clock limit of each call.
    pub timeout: Duration,
    /// Today (UTC), which picks the eval ledger's month.
    pub today: CalendarDate,
}

/// A path of an [`EvalRequest`] that must be absolute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathField {
    /// [`EvalRequest::corpus_dir`].
    CorpusDir,
    /// [`EvalRequest::system_prompt_file`].
    SystemPromptFile,
}

impl fmt::Display for PathField {
    /// The field's name: `corpus_dir` or `system_prompt_file`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CorpusDir => "corpus_dir",
            Self::SystemPromptFile => "system_prompt_file",
        })
    }
}

/// Why an evaluation did not complete. Whatever was spent before it stopped is charged.
#[derive(Debug, Error)]
pub enum EvalError<R, L> {
    /// The split holds no case.
    #[error("the split holds no case")]
    NoCases,
    /// A request path is relative; no call was made.
    #[error("the request's {field} is not an absolute path")]
    RelativePath {
        /// The path at fault.
        field: PathField,
    },
    /// A case's diff does not make a valid prompt; no call was made.
    #[error("case {case:?} does not make a valid prompt")]
    Prompt {
        /// The case id.
        case: String,
        /// Why the prompt was refused.
        #[source]
        source: InvalidPrompt,
    },
    /// The runner could not make or supervise a call of `case`. The call was charged its cap
    /// per attempt, and no further call was made: a broken runner would fail every call.
    #[error("the model runner failed on case {case:?}")]
    Runner {
        /// The case id.
        case: String,
        /// The runner's error.
        #[source]
        source: R,
    },
    /// The subscription's usage limit refused a call of `case`. The call was charged, and no
    /// further call was made: every later call would meet the same limit.
    #[error("the usage limit refused a call on case {case:?}")]
    QuotaExhausted {
        /// The case id.
        case: String,
    },
    /// The eval ledger cannot fit the next call: `spent` plus the call's worst case would pass
    /// `cap` in `month`. Nothing is drawn from the production cap instead.
    #[error("the eval ledger is exhausted for {month}: {spent} of {cap} AEU spent")]
    Exhausted {
        /// The ledger month.
        month: EvalMonth,
        /// What evaluations had spent in it, this one included.
        spent: Aeu,
        /// The monthly cap.
        cap: Aeu,
    },
    /// The eval ledger could not be read or charged.
    #[error("the eval ledger for {month} could not be read or charged")]
    Ledger {
        /// The ledger month.
        month: EvalMonth,
        /// The port's error.
        #[source]
        source: L,
    },
}

/// How one call ended, for the evaluation loop.
enum Called<E> {
    /// It ran and was scored.
    Scored(RunScore),
    /// The runner failed; the evaluation stops.
    Failed(E),
    /// The usage limit refused it; the evaluation stops.
    Quota,
}

/// The most one call can cost: the first attempt and its one retry, each at the role's cap.
fn worst_call() -> Aeu {
    EVAL_ROLE.cap().saturating_add(EVAL_ROLE.cap())
}

/// Runs every case of the request's split [`EVAL_RUNS`] times through `runner` and scores
/// each run, charging every call's cost to `ledger` before the next call.
///
/// No call starts unless the month's eval spend plus the call's worst case (two attempts at
/// the `combined` cap) stays within [`EVAL_MONTHLY_CAP`]; the evaluation then stops with
/// [`EvalError::Exhausted`]. A run that cannot be scored counts against the change; a runner
/// failure or the usage limit stops the evaluation once the call is charged.
///
/// # Errors
/// [`EvalError`] when the split is empty, a request path is relative or a prompt is invalid
/// (all before any call), or when the runner fails, the usage limit is reached, or the eval
/// ledger is exhausted or fails.
pub fn run_eval<R: ModelRunner, L: EvalLedger>(
    runner: &R,
    ledger: &mut L,
    request: &EvalRequest<'_>,
) -> Result<EvalReport, EvalError<R::Error, L::Error>> {
    let prompts = prepare(request)?;
    let month = EvalMonth::of(request.today);
    let mut spent = ledger
        .spent(month)
        .map_err(|source| EvalError::Ledger { month, source })?;
    let mut charged = Aeu::ZERO;
    let cases = request.corpus.cases();
    let mut outcomes = Vec::with_capacity(cases.len());
    for (case, prompt) in cases.iter().zip(prompts) {
        let call = case_call(request, case, prompt);
        let mut runs = [RunScore::Missed; EVAL_RUNS];
        for run in &mut runs {
            if spent.saturating_add(worst_call()) > EVAL_MONTHLY_CAP {
                let cap = EVAL_MONTHLY_CAP;
                return Err(EvalError::Exhausted { month, spent, cap });
            }
            let (called, cost) = make_call(runner, &call, case, request.class_map);
            ledger
                .charge(month, cost)
                .map_err(|source| EvalError::Ledger { month, source })?;
            spent = spent.saturating_add(cost);
            charged = charged.saturating_add(cost);
            *run = match called {
                Called::Scored(score) => score,
                Called::Failed(source) => {
                    let case = case.id().to_owned();
                    return Err(EvalError::Runner { case, source });
                }
                Called::Quota => {
                    let case = case.id().to_owned();
                    return Err(EvalError::QuotaExhausted { case });
                }
            };
        }
        outcomes.push(outcome(case, runs));
    }
    Ok(EvalReport {
        split: request.corpus.split(),
        bundle: request.bundle.hash(),
        month,
        spent: charged,
        cases: outcomes,
    })
}

/// Everything checked before the first call: a case to run, absolute paths, and each case's
/// prompt, in case order.
fn prepare<R, L>(request: &EvalRequest<'_>) -> Result<Vec<Prompt>, EvalError<R, L>> {
    let cases = request.corpus.cases();
    if cases.is_empty() {
        return Err(EvalError::NoCases);
    }
    for (path, field) in [
        (request.corpus_dir, PathField::CorpusDir),
        (request.system_prompt_file, PathField::SystemPromptFile),
    ] {
        if !path.is_absolute() {
            return Err(EvalError::RelativePath { field });
        }
    }
    cases
        .iter()
        .map(|case| {
            let pack = format!("Diff:\n{}", case.diff());
            request
                .bundle
                .user_prompt(EVAL_ROLE, &pack)
                .map_err(|source| EvalError::Prompt {
                    case: case.id().to_owned(),
                    source,
                })
        })
        .collect()
}

/// The call that reviews `case`: its diff as the pack, its tree as the only readable directory.
fn case_call(request: &EvalRequest<'_>, case: &EvalCase, prompt: Prompt) -> ModelCall {
    ModelCall {
        system_prompt_file: request.system_prompt_file.to_path_buf(),
        user_prompt: prompt,
        json_schema: Contract::Findings.schema_text().to_owned(),
        add_dir: request.corpus_dir.join(case.id()).join("tree"),
        max_turns: EVAL_ROLE.max_turns(),
        budget: EVAL_ROLE.cap(),
        timeout: request.timeout,
    }
}

/// One run of `call` and what it cost. A runner failure is charged the role's cap for each
/// attempt that may have started, as the review stages do.
fn make_call<R: ModelRunner>(
    runner: &R,
    call: &ModelCall,
    case: &EvalCase,
    map: &ClassMap,
) -> (Called<R::Error>, Aeu) {
    match run_with_retry(runner, call) {
        Ok(attempts) => {
            let called =
                score_result(attempts.last(), case, map).map_or(Called::Quota, Called::Scored);
            (called, attempts.cost())
        }
        Err(RetryError::First(source)) => (Called::Failed(source), call.budget),
        Err(RetryError::Retry { first, source }) => (
            Called::Failed(source),
            first.cost.saturating_add(call.budget),
        ),
    }
}

/// Scores one call's final result against `case`'s labels; `None` when the usage limit
/// refused it, which is not a score but the end of the evaluation.
fn score_result(result: &ModelResult, case: &EvalCase, map: &ClassMap) -> Option<RunScore> {
    let unscorable = |reason| Some(RunScore::Unscorable(reason));
    if result.outcome == ModelOutcome::QuotaExhausted {
        return None;
    }
    if !result.init.as_ref().is_ok_and(SessionInit::is_confined) {
        return unscorable(Unscorable::Unconfined);
    }
    let structured = match &result.outcome {
        ModelOutcome::Ok(structured) => structured,
        ModelOutcome::Malformed => return unscorable(Unscorable::Malformed),
        ModelOutcome::BudgetExhausted => return unscorable(Unscorable::BudgetExhausted),
        ModelOutcome::TurnsExhausted => return unscorable(Unscorable::TurnsExhausted),
        ModelOutcome::Timeout => return unscorable(Unscorable::Timeout),
        ModelOutcome::QuotaExhausted => return None,
    };
    let findings = match score::read_findings(structured.as_value()) {
        Ok(findings) => findings,
        Err(problem) => return unscorable(Unscorable::Output(problem)),
    };
    if case.labels().is_empty() {
        return Some(match findings.len() {
            0 => RunScore::Quiet,
            count => RunScore::Noisy { findings: count },
        });
    }
    let detected = case.labels().iter().any(|label| {
        findings
            .iter()
            .any(|finding| score::matches(label, finding, map))
    });
    Some(if detected {
        RunScore::Detected
    } else {
        RunScore::Missed
    })
}

/// `case`'s outcome over its runs.
fn outcome(case: &EvalCase, runs: [RunScore; EVAL_RUNS]) -> CaseOutcome {
    let kind = if case.labels().is_empty() {
        CaseKind::TrueNegative
    } else {
        CaseKind::Defect
    };
    CaseOutcome {
        id: case.id().to_owned(),
        // The report outlives the corpus it was run from, so it keeps its own copy.
        authoring_model: case.authoring_model().clone(),
        source: case.source(),
        content: case.content(),
        kind,
        runs,
    }
}
