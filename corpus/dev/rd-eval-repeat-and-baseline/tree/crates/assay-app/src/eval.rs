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

use std::path::Path;
use std::time::Duration;

use assay_domain::Aeu;
use thiserror::Error;

use crate::calendar::CalendarDate;
use crate::contract::Contract;
use crate::model_runner::{
    ModelCall, ModelOutcome, ModelResult, ModelRunner, Prompt, RetryError, run_with_retry,
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

/// Why an evaluation did not complete. Whatever was spent before it stopped is charged.
#[derive(Debug, Error)]
pub enum EvalError<L> {
    /// The split holds no case.
    #[error("the split holds no case")]
    NoCases,
    /// A case's diff does not make a valid prompt; no call was made.
    #[error("case {case:?} does not make a valid prompt")]
    Prompt {
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

/// The most one call can cost: the first attempt and its one retry, each at the role's cap.
fn worst_call() -> Aeu {
    EVAL_ROLE.cap().saturating_add(EVAL_ROLE.cap())
}

/// Runs every case of the request's split [`EVAL_RUNS`] times through `runner` and scores
/// each run, charging every call's cost to `ledger` before the next call.
///
/// No call starts unless the month's eval spend plus the call's worst case (two attempts at
/// the `combined` cap) stays within [`EVAL_MONTHLY_CAP`]; the evaluation then stops with
/// [`EvalError::Exhausted`]. A run that cannot be scored counts against the change.
///
/// # Errors
/// [`EvalError`] when the split is empty, a prompt is invalid, the eval ledger is exhausted or
/// fails; prompts are built before any call.
pub fn run_eval<R: ModelRunner, L: EvalLedger>(
    runner: &R,
    ledger: &mut L,
    request: &EvalRequest<'_>,
) -> Result<EvalReport, EvalError<L::Error>> {
    let cases = request.corpus.cases();
    if cases.is_empty() {
        return Err(EvalError::NoCases);
    }
    let prompts = cases
        .iter()
        .map(|case| {
            let pack = format!("Diff:\n{}", case.diff());
            request
                .bundle
                .user_prompt(EVAL_ROLE, &pack)
                .map_err(|_invalid| EvalError::Prompt {
                    case: case.id().to_owned(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let month = EvalMonth::of(request.today);
    let mut spent = ledger
        .spent(month)
        .map_err(|source| EvalError::Ledger { month, source })?;
    let mut charged = Aeu::ZERO;
    let mut outcomes = Vec::with_capacity(cases.len());
    for (case, prompt) in cases.iter().zip(prompts) {
        let call = case_call(request, case, prompt);
        let mut runs = [RunScore::Missed; EVAL_RUNS];
        for run in &mut runs {
            if spent.saturating_add(worst_call()) > EVAL_MONTHLY_CAP {
                let cap = EVAL_MONTHLY_CAP;
                return Err(EvalError::Exhausted { month, spent, cap });
            }
            let (score, cost) = score_call(runner, &call, case, request.class_map);
            ledger
                .charge(month, cost)
                .map_err(|source| EvalError::Ledger { month, source })?;
            spent = spent.saturating_add(cost);
            charged = charged.saturating_add(cost);
            *run = score;
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

/// One run of `call`, scored, and what it cost. A runner failure is charged the role's cap
/// for each attempt that may have started, as the review stages do.
fn score_call<R: ModelRunner>(
    runner: &R,
    call: &ModelCall,
    case: &EvalCase,
    map: &ClassMap,
) -> (RunScore, Aeu) {
    match run_with_retry(runner, call) {
        Ok(attempts) => (score_result(attempts.last(), case, map), attempts.cost()),
        Err(RetryError::First(_error)) => {
            (RunScore::Unscorable(Unscorable::CallFailed), call.budget)
        }
        Err(RetryError::Retry { first, .. }) => (
            RunScore::Unscorable(Unscorable::CallFailed),
            first.cost.saturating_add(call.budget),
        ),
    }
}

/// Scores one call's final result against `case`'s labels.
fn score_result(result: &ModelResult, case: &EvalCase, map: &ClassMap) -> RunScore {
    let unscorable = RunScore::Unscorable;
    if !result.init.as_ref().is_ok_and(SessionInit::is_confined) {
        return unscorable(Unscorable::Unconfined);
    }
    let structured = match &result.outcome {
        ModelOutcome::Ok(structured) => structured,
        ModelOutcome::Malformed => return unscorable(Unscorable::Malformed),
        ModelOutcome::BudgetExhausted => return unscorable(Unscorable::BudgetExhausted),
        ModelOutcome::TurnsExhausted => return unscorable(Unscorable::TurnsExhausted),
        ModelOutcome::Timeout => return unscorable(Unscorable::Timeout),
        ModelOutcome::QuotaExhausted => return unscorable(Unscorable::QuotaExhausted),
    };
    let findings = match score::read_findings(structured.as_value()) {
        Ok(findings) => findings,
        Err(problem) => return unscorable(Unscorable::Output(problem)),
    };
    if case.labels().is_empty() {
        return match findings.len() {
            0 => RunScore::Quiet,
            count => RunScore::Noisy { findings: count },
        };
    }
    let detected = case.labels().iter().any(|label| {
        findings
            .iter()
            .any(|finding| score::matches(label, finding, map))
    });
    if detected {
        RunScore::Detected
    } else {
        RunScore::Missed
    }
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
        kind,
        runs,
    }
}
