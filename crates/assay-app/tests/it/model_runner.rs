//! The `ModelRunner` port and its retry policy (`docs/architecture/03-contracts.md` §7), driven
//! through a scripted in-memory runner (TST-09).

use std::assert_matches;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU8;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    InitReadError, InvalidPrompt, ModelCall, ModelOutcome, ModelResult, ModelRunner, Prompt,
    PromptError, RetryError, Structured, run_with_retry,
};
use assay_domain::Aeu;
use serde_json::json;

/// The runner ran out of scripted results.
#[derive(Debug, PartialEq, Eq)]
struct OutOfScript;

impl fmt::Display for OutOfScript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no scripted result left")
    }
}

impl Error for OutOfScript {}

/// Answers each call with the next scripted result and counts the calls.
#[derive(Debug)]
struct Scripted {
    results: RefCell<VecDeque<ModelResult>>,
    calls: Cell<usize>,
}

impl Scripted {
    fn new(results: impl IntoIterator<Item = ModelResult>) -> Self {
        Self {
            results: RefCell::new(results.into_iter().collect()),
            calls: Cell::new(0),
        }
    }
}

impl ModelRunner for Scripted {
    type Error = OutOfScript;

    fn run(&self, _call: &ModelCall) -> Result<ModelResult, OutOfScript> {
        self.calls.set(self.calls.get() + 1);
        self.results.borrow_mut().pop_front().ok_or(OutOfScript)
    }
}

fn call() -> Result<ModelCall, Box<dyn Error>> {
    Ok(ModelCall {
        system_prompt_file: PathBuf::from("/bundle/system.md"),
        user_prompt: Prompt::new("Context pack, then the role instruction.")?,
        json_schema: r#"{"type":"object"}"#.to_owned(),
        add_dir: PathBuf::from("/work/pr"),
        max_turns: NonZeroU8::new(8).ok_or("8 is not zero")?,
        budget: Aeu::from_micros(740_000),
        timeout: Duration::from_secs(600),
    })
}

fn result(outcome: ModelOutcome, micros: u64) -> ModelResult {
    ModelResult {
        outcome,
        cost: Aeu::from_micros(micros),
        rate_limits: Vec::new(),
        init: Err(InitReadError::Missing),
    }
}

fn ok() -> ModelOutcome {
    ModelOutcome::Ok(Structured::new(json!({"findings": []})))
}

#[test]
fn malformed_output_is_retried_once_and_both_attempts_are_charged() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::new([result(ModelOutcome::Malformed, 100), result(ok(), 200)]);

    let attempts = run_with_retry(&runner, &call()?)?;

    assert_eq!(runner.calls.get(), 2);
    assert_eq!(attempts.first, result(ModelOutcome::Malformed, 100));
    assert_eq!(attempts.retry, Some(result(ok(), 200)));
    assert_eq!(attempts.last().outcome, ok());
    assert_eq!(attempts.cost(), Aeu::from_micros(300));
    Ok(())
}

#[test]
fn a_second_malformed_output_is_not_retried_again() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::new([
        result(ModelOutcome::Malformed, 100),
        result(ModelOutcome::Malformed, 100),
        result(ok(), 100),
    ]);

    let attempts = run_with_retry(&runner, &call()?)?;

    assert_eq!(runner.calls.get(), 2);
    assert_eq!(attempts.last().outcome, ModelOutcome::Malformed);
    Ok(())
}

#[test]
fn every_other_outcome_is_final() -> Result<(), Box<dyn Error>> {
    let finals = [
        ok(),
        ModelOutcome::BudgetExhausted,
        ModelOutcome::TurnsExhausted,
        ModelOutcome::Timeout,
        ModelOutcome::QuotaExhausted,
    ];
    for outcome in finals {
        let runner = Scripted::new([result(outcome.clone(), 740_000), result(ok(), 1)]);

        let attempts = run_with_retry(&runner, &call()?)?;

        assert_eq!(runner.calls.get(), 1, "{outcome:?} was retried");
        assert_eq!(attempts.first, result(outcome, 740_000));
        assert_eq!(attempts.retry, None);
    }
    Ok(())
}

#[test]
fn a_failed_retry_hands_back_the_charged_first_attempt() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::new([result(ModelOutcome::Malformed, 100)]);

    let failed = run_with_retry(&runner, &call()?);

    assert_eq!(
        failed,
        Err(RetryError::Retry {
            first: result(ModelOutcome::Malformed, 100),
            source: OutOfScript,
        })
    );
    Ok(())
}

#[test]
fn a_failed_first_call_is_not_retried() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::new([]);

    let failed = run_with_retry(&runner, &call()?);

    assert_eq!(runner.calls.get(), 1);
    assert_eq!(failed, Err(RetryError::First(OutOfScript)));
    Ok(())
}

#[test]
fn a_prompt_cannot_be_empty_start_with_a_dash_or_hold_a_nul() {
    let reason = |text: &str| Prompt::new(text).map_err(|invalid| invalid.reason());

    assert_eq!(reason(""), Err(PromptError::Empty));
    assert_eq!(reason("--bare"), Err(PromptError::LeadingDash));
    assert_eq!(reason("a\0b"), Err(PromptError::Nul));
    assert_matches!(Prompt::new("review - this"), Ok(prompt) if prompt.as_str() == "review - this");
}

#[test]
fn a_rejected_prompt_hands_its_text_back() {
    let rejected = Prompt::new("-x".to_owned()).map_err(InvalidPrompt::into_inner);

    assert_eq!(rejected, Err("-x".to_owned()));
}
