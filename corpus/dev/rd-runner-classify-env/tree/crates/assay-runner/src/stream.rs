//! Classification of a `--output-format stream-json --verbose` transcript (03 §7, §8).
//!
//! Each line is one JSON message. Only three kinds matter: `rate_limit_event` (kept when its
//! status is `rejected` or `allowed_warning`), `assistant` (whose `error: "rate_limit"` marks a
//! usage-limit refusal) and the final `result`, which alone carries `total_cost_usd` and the
//! structured output. Every other message is skipped, so new message kinds do not break a call.

mod message;

use assay_app::{ModelOutcome, ModelResult, RateLimitObserved, RateLimitStatus, Structured};
use assay_domain::Aeu;

use message::{Message, ResultKind, ResultMessage};

/// How the process behind a transcript stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// It exited on its own.
    Exited,
    /// It was killed at its wall-clock limit.
    TimedOut,
    /// It was killed for writing more than the output cap; the transcript is cut short.
    OutputCapped,
}

/// The outcome, cost and rate limits of the call behind `transcript`.
///
/// The cost is the last `result`'s `total_cost_usd`, or `budget` when the call timed out, was
/// cut short, or reported no readable cost (03 §8: on timeout the cap is charged).
pub(crate) fn classify(transcript: &[u8], ending: Ending, budget: Aeu) -> ModelResult {
    let Scan {
        result,
        rate_limits,
        rate_limited,
        unreadable,
    } = Scan::of(transcript);
    let (outcome, cost) = match ending {
        Ending::TimedOut => (ModelOutcome::Timeout, budget),
        Ending::OutputCapped => (ModelOutcome::Malformed, budget),
        Ending::Exited => {
            let cost = result
                .as_ref()
                .and_then(|result| result.cost)
                .unwrap_or(budget);
            let outcome = if unreadable {
                ModelOutcome::Malformed
            } else {
                outcome(result, rate_limited)
            };
            (outcome, cost)
        }
    };
    ModelResult {
        outcome,
        cost,
        rate_limits,
    }
}

/// What a transcript holds, as far as classification needs it.
#[derive(Debug, Default)]
struct Scan {
    /// The last `result` message.
    result: Option<ResultMessage>,
    /// The `rejected` and `allowed_warning` rate-limit events, in order.
    rate_limits: Vec<RateLimitObserved>,
    /// A rate-limit event was `rejected` or an assistant message reported `rate_limit`.
    rate_limited: bool,
    /// A non-empty line was not a JSON message.
    unreadable: bool,
}

impl Scan {
    fn of(transcript: &[u8]) -> Self {
        let mut scan = Self::default();
        let lines = transcript
            .split(|&byte| byte == b'\n')
            .filter(|line| !line.trim_ascii().is_empty());
        for line in lines {
            match Message::parse(line) {
                Ok(Message::RateLimit(Some(observed))) => {
                    scan.rate_limited |= observed.status == RateLimitStatus::Rejected;
                    scan.rate_limits.push(observed);
                }
                Ok(Message::Assistant { rate_limited }) => scan.rate_limited |= rate_limited,
                Ok(Message::Result(result)) => scan.result = Some(result),
                Ok(Message::RateLimit(None) | Message::Other) => {}
                Err(_) => scan.unreadable = true,
            }
        }
        scan
    }
}

/// The outcome of a call that exited with a readable transcript.
fn outcome(result: Option<ResultMessage>, rate_limited: bool) -> ModelOutcome {
    let Some(result) = result else {
        return if rate_limited {
            ModelOutcome::QuotaExhausted
        } else {
            ModelOutcome::Malformed
        };
    };
    match result.kind {
        ResultKind::MaxBudget => ModelOutcome::BudgetExhausted,
        ResultKind::MaxTurns => ModelOutcome::TurnsExhausted,
        ResultKind::Success => match result.structured {
            Some(value) if !value.is_null() => ModelOutcome::Ok(Structured::new(value)),
            _ => ModelOutcome::Malformed,
        },
        ResultKind::Error if rate_limited || result.usage_limit => ModelOutcome::QuotaExhausted,
        ResultKind::Error => ModelOutcome::Malformed,
    }
}
