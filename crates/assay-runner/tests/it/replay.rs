//! `Replay` classifies recorded streams exactly as `ClaudeHeadless` classifies live ones: one
//! fixture per outcome (03 §7, §8).

use std::error::Error;

use assay_app::{
    ModelOutcome, ModelResult, ModelRunner, RateLimitObserved, RateLimitStatus, Structured,
};
use assay_domain::Aeu;
use assay_runner::{Recording, Replay, ReplayError};
use serde_json::json;

use crate::support::{BUDGET, call, stream};

/// Replays the completed stream `name` once.
fn replay(name: &str) -> Result<ModelResult, Box<dyn Error>> {
    let runner = Replay::new([Recording::Completed(stream(name)?)]);
    Ok(runner.run(&call()?)?)
}

#[test]
fn a_successful_result_yields_its_structured_output_and_cost() -> Result<(), Box<dyn Error>> {
    let result = replay("ok")?;

    assert_eq!(
        result,
        ModelResult {
            outcome: ModelOutcome::Ok(Structured::new(json!({
                "findings": [{"title": "unwrap in library code", "severity": "major"}]
            }))),
            cost: Aeu::from_micros(421_300),
            rate_limits: Vec::new(),
        }
    );
    Ok(())
}

#[test]
fn a_max_budget_result_is_budget_exhausted_at_its_real_cost() -> Result<(), Box<dyn Error>> {
    let result = replay("budget_exhausted")?;

    assert_eq!(result.outcome, ModelOutcome::BudgetExhausted);
    assert_eq!(result.cost, Aeu::from_micros(751_200));
    Ok(())
}

#[test]
fn a_max_turns_result_is_turns_exhausted_at_its_real_cost() -> Result<(), Box<dyn Error>> {
    let result = replay("turns_exhausted")?;

    assert_eq!(result.outcome, ModelOutcome::TurnsExhausted);
    assert_eq!(result.cost, Aeu::from_micros(330_100));
    Ok(())
}

#[test]
fn a_usage_limit_refusal_is_quota_exhausted() -> Result<(), Box<dyn Error>> {
    let result = replay("quota_exhausted")?;

    assert_eq!(
        result,
        ModelResult {
            outcome: ModelOutcome::QuotaExhausted,
            cost: Aeu::ZERO,
            rate_limits: vec![RateLimitObserved {
                status: RateLimitStatus::Rejected,
                resets_at: Some(1_791_240_000),
            }],
        }
    );
    Ok(())
}

#[test]
fn a_usage_limit_result_alone_is_quota_exhausted() -> Result<(), Box<dyn Error>> {
    let result = replay("usage_limit_result_only")?;

    assert_eq!(result.outcome, ModelOutcome::QuotaExhausted);
    Ok(())
}

#[test]
fn only_warning_and_rejected_rate_limit_events_are_observed() -> Result<(), Box<dyn Error>> {
    let result = replay("rate_limit_warning")?;

    assert_eq!(
        result.rate_limits,
        [RateLimitObserved {
            status: RateLimitStatus::AllowedWarning,
            resets_at: Some(1_791_238_000),
        }]
    );
    assert_eq!(result.cost, Aeu::from_micros(421_300));
    Ok(())
}

#[test]
fn a_stream_without_a_result_is_malformed_and_charged_the_cap() -> Result<(), Box<dyn Error>> {
    let result = replay("no_result")?;

    assert_eq!(result.outcome, ModelOutcome::Malformed);
    assert_eq!(result.cost, BUDGET);
    Ok(())
}

#[test]
fn a_line_that_is_not_json_makes_the_stream_malformed() -> Result<(), Box<dyn Error>> {
    let result = replay("garbage_line")?;

    assert_eq!(result.outcome, ModelOutcome::Malformed);
    assert_eq!(result.cost, Aeu::from_micros(421_300));
    Ok(())
}

#[test]
fn a_success_without_structured_output_is_malformed_at_its_cost() -> Result<(), Box<dyn Error>> {
    let result = replay("no_structured_output")?;

    assert_eq!(result.outcome, ModelOutcome::Malformed);
    assert_eq!(result.cost, Aeu::from_micros(120_000));
    Ok(())
}

#[test]
fn an_execution_error_is_malformed_at_its_cost() -> Result<(), Box<dyn Error>> {
    let result = replay("error_during_execution")?;

    assert_eq!(result.outcome, ModelOutcome::Malformed);
    assert_eq!(result.cost, Aeu::from_micros(50_000));
    Ok(())
}

#[test]
fn a_timed_out_recording_is_charged_the_cap_and_keeps_its_rate_limits() -> Result<(), Box<dyn Error>>
{
    let runner = Replay::new([Recording::TimedOut(stream("partial")?)]);

    let result = runner.run(&call()?)?;

    assert_eq!(
        result,
        ModelResult {
            outcome: ModelOutcome::Timeout,
            cost: BUDGET,
            rate_limits: vec![RateLimitObserved {
                status: RateLimitStatus::AllowedWarning,
                resets_at: None,
            }],
        }
    );
    Ok(())
}

#[test]
fn a_timeout_is_charged_the_cap_even_after_a_result() -> Result<(), Box<dyn Error>> {
    let runner = Replay::new([Recording::TimedOut(stream("ok")?)]);

    let result = runner.run(&call()?)?;

    assert_eq!(
        (result.outcome, result.cost),
        (ModelOutcome::Timeout, BUDGET)
    );
    Ok(())
}

#[test]
fn recordings_are_replayed_in_order_and_then_run_out() -> Result<(), Box<dyn Error>> {
    let runner = Replay::new([
        Recording::Completed(stream("no_result")?),
        Recording::Completed(stream("ok")?),
    ]);
    let call = call()?;

    let first = runner.run(&call)?.outcome;
    let second = runner.run(&call)?.cost;
    let third = runner.run(&call);

    assert_eq!(first, ModelOutcome::Malformed);
    assert_eq!(second, Aeu::from_micros(421_300));
    assert_eq!(third, Err(ReplayError::Exhausted { recorded: 2 }));
    Ok(())
}
