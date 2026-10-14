//! Classification of a `--output-format stream-json --verbose` transcript (03 §7, §8).
//!
//! Each line is one JSON message. Only three kinds matter: `rate_limit_event` (kept when its
//! status is `rejected` or `allowed_warning`), `assistant` (whose `error: "rate_limit"` marks a
//! usage-limit refusal) and the final `result`, which alone carries `total_cost_usd` and the
//! structured output. Every other message is skipped, so new message kinds do not break a call.

mod init;
mod message;

pub use init::read_session_init;

use assay_app::{ModelOutcome, ModelResult, RateLimitObserved, Structured};
use assay_domain::{Aeu, RateLimitStatus};

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
    let mut scan = Scan::of(transcript);
    let rate_limits = std::mem::take(&mut scan.rate_limits);
    let (outcome, cost) = match ending {
        Ending::TimedOut => (ModelOutcome::Timeout, budget),
        Ending::OutputCapped => (ModelOutcome::Malformed, budget),
        Ending::Exited => {
            let cost = scan
                .result
                .as_ref()
                .and_then(|result| result.cost)
                .unwrap_or(budget);
            (scan.outcome(), cost)
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

    /// The outcome of a call that exited on its own, decided by its `result` first.
    ///
    /// Budget, turn and quota endings are final whatever else the transcript holds: a stray line
    /// on stdout must not turn them into a retried `Malformed`. An unreadable line only stops a
    /// success from counting, since its output may then be incomplete.
    fn outcome(self) -> ModelOutcome {
        let Some(result) = self.result else {
            return if self.rate_limited {
                ModelOutcome::QuotaExhausted
            } else {
                ModelOutcome::Malformed
            };
        };
        match result.kind {
            ResultKind::MaxBudget => ModelOutcome::BudgetExhausted,
            ResultKind::MaxTurns => ModelOutcome::TurnsExhausted,
            ResultKind::Success => match result.structured {
                Some(value) if !value.is_null() && !self.unreadable => {
                    ModelOutcome::Ok(Structured::new(value))
                }
                _ => ModelOutcome::Malformed,
            },
            ResultKind::Error if self.rate_limited || result.usage_limit => {
                ModelOutcome::QuotaExhausted
            }
            ResultKind::Error => ModelOutcome::Malformed,
        }
    }
}

#[cfg(test)]
mod tests {
    use assay_app::ModelOutcome;
    use assay_domain::Aeu;

    use super::{Ending, classify};

    const BUDGET: Aeu = Aeu::from_micros(740_000);
    const STRAY: &str = "(node) Warning: printed to stdout";

    fn outcome_of(lines: &[&str]) -> ModelOutcome {
        classify(lines.join("\n").as_bytes(), Ending::Exited, BUDGET).outcome
    }

    #[test]
    fn a_stray_line_does_not_hide_a_turns_exhausted_result() {
        let result =
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"total_cost_usd":0.3}"#;

        assert_eq!(outcome_of(&[STRAY, result]), ModelOutcome::TurnsExhausted);
    }

    #[test]
    fn a_stray_line_does_not_hide_a_usage_limit_result() {
        let result = r#"{"type":"result","subtype":"success","is_error":true,"total_cost_usd":0,"api_error":"usage_limit_reached"}"#;

        assert_eq!(outcome_of(&[STRAY, result]), ModelOutcome::QuotaExhausted);
    }

    #[test]
    fn a_stray_line_does_not_hide_a_rejection_without_a_result() {
        let rejected = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#;

        assert_eq!(outcome_of(&[rejected, STRAY]), ModelOutcome::QuotaExhausted);
    }

    #[test]
    fn a_stray_line_makes_a_successful_result_malformed() {
        let result = r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.1,"structured_output":{"findings":[]}}"#;

        assert_eq!(outcome_of(&[STRAY, result]), ModelOutcome::Malformed);
    }
}
