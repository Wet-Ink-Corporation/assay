//! The model runner port (`docs/architecture/03-contracts.md` §7): one headless model call in, its
//! outcome, real cost and observed rate limits out. Adapters live in `assay-runner`.

mod prompt;
mod retry;

use std::num::NonZeroU8;
use std::path::PathBuf;
use std::time::Duration;

use assay_domain::{Aeu, RateLimitStatus};
use serde_json::Value;

use crate::session_init::{InitReadError, SessionInit};

pub use prompt::{InvalidPrompt, Prompt, PromptError};
pub use retry::{ModelAttempts, RetryError, run_with_retry};

/// Runs one model call (03 §7). Implemented by `ClaudeHeadless` and `Replay` in `assay-runner`.
///
/// A call that ran yields `Ok` with its [`ModelOutcome`] and cost, whether or not the model
/// produced output: budget, turn, time and quota exhaustion and malformed output are outcomes,
/// not errors. `Err` means the call could not be made or supervised; when it fails after the
/// model may have started, its cost is unknown and the caller charges the call's budget.
pub trait ModelRunner {
    /// Why a call could not be made or supervised.
    type Error: std::error::Error;

    /// Makes `call` once. The retry policy is [`run_with_retry`]'s, not the adapter's.
    ///
    /// # Errors
    /// Returns [`Self::Error`] when the call could not be started or supervised.
    fn run(&self, call: &ModelCall) -> Result<ModelResult, Self::Error>;
}

/// One model call (03 §7). The tool set is not a field: the headless profile fixes it to
/// `Read,Grep,Glob` (03 §8), so no call can widen it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCall {
    /// The base-sourced system prompt bundle appended to the CLI's own (absolute path).
    pub system_prompt_file: PathBuf,
    /// The context pack first, then the role instruction.
    pub user_prompt: Prompt,
    /// The JSON Schema the structured output must satisfy, as JSON text.
    pub json_schema: String,
    /// The read-only PR tree the model may read (absolute path).
    pub add_dir: PathBuf,
    /// The CLI's turn limit (`--max-turns`).
    pub max_turns: NonZeroU8,
    /// The call's cap, passed as `--max-budget-usd` exactly and charged when the real cost is
    /// unknown.
    pub budget: Aeu,
    /// The wall-clock limit of the whole call.
    pub timeout: Duration,
}

/// What one model call came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResult {
    /// How the call ended.
    pub outcome: ModelOutcome,
    /// The cost to ledger: the CLI's `total_cost_usd` from the final `result` message, or the
    /// call's budget when the call timed out or reported no cost.
    pub cost: Aeu,
    /// The `rejected` and `allowed_warning` rate-limit events of the call, in stream order.
    pub rate_limits: Vec<RateLimitObserved>,
    /// The session's `system/init`, or why it could not be read. The preflight judges the first
    /// call's ([`AuthProof::check_first_call`](crate::AuthProof::check_first_call)); a call
    /// whose init is not clean must not be used.
    pub init: Result<SessionInit, InitReadError>,
}

/// How a model call ended (03 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelOutcome {
    /// The model produced structured output.
    Ok(Structured),
    /// The CLI stopped at `--max-budget-usd` (`error_max_budget_usd`). Not retried.
    BudgetExhausted,
    /// The CLI stopped at `--max-turns` (`error_max_turns`). Not retried.
    TurnsExhausted,
    /// The wall-clock limit passed; the call is charged at its budget. Not retried.
    Timeout,
    /// The subscription's usage limit refused the call. Not retried.
    QuotaExhausted,
    /// The output was not a well-formed stream ending in structured output. Retried once.
    Malformed,
}

/// The model's structured output, unvalidated: it is untrusted until the mechanical validation
/// downstream accepts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Structured(Value);

impl Structured {
    /// Wraps the `structured_output` of a `result` message.
    #[must_use]
    pub const fn new(value: Value) -> Self {
        Self(value)
    }

    /// The output as JSON.
    #[must_use]
    pub const fn as_value(&self) -> &Value {
        &self.0
    }

    /// The output as owned JSON.
    #[must_use]
    pub fn into_value(self) -> Value {
        self.0
    }
}

/// A `rate_limit_event` worth acting on (03 §8, `06-operations.md` §1): the stream reports
/// `allowed` routinely, so only these two statuses are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitObserved {
    /// The limit's status.
    pub status: RateLimitStatus,
    /// When the limit resets, in Unix seconds, when the event says.
    pub resets_at: Option<u64>,
}
