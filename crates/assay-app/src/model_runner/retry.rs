//! The retry policy of a model call (03 §7): malformed output is retried exactly once.

use assay_domain::Aeu;

use super::{ModelCall, ModelOutcome, ModelResult, ModelRunner};

/// The attempts of one model call: the first, and the retry when the first was malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAttempts {
    /// The first attempt.
    pub first: ModelResult,
    /// The retry, made only when the first attempt was [`ModelOutcome::Malformed`].
    pub retry: Option<ModelResult>,
}

impl ModelAttempts {
    /// The attempt that decides the call: the retry when there was one, else the first.
    #[must_use]
    pub fn last(&self) -> &ModelResult {
        self.retry.as_ref().unwrap_or(&self.first)
    }

    /// The cost of every attempt, to ledger.
    #[must_use]
    pub fn cost(&self) -> Aeu {
        self.retry.as_ref().map_or(self.first.cost, |retry| {
            self.first.cost.saturating_add(retry.cost)
        })
    }
}

/// A model call that could not be made or supervised.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RetryError<E> {
    /// The first attempt failed; nothing was retried.
    #[error("the model call failed")]
    First(#[source] E),
    /// The retry failed after a malformed first attempt, which is handed back so its cost is
    /// still ledgered.
    #[error("the retry of a malformed model call failed")]
    Retry {
        /// The malformed first attempt.
        first: ModelResult,
        /// Why the retry failed.
        #[source]
        source: E,
    },
}

/// Makes `call` through `runner`, retrying a [`ModelOutcome::Malformed`] first attempt once.
///
/// Budget, turn, time and quota exhaustion are final (03 §7), and so is a second malformed
/// attempt.
///
/// # Errors
/// Returns [`RetryError::First`] when the first attempt fails, and [`RetryError::Retry`] with
/// the first attempt when the retry fails.
pub fn run_with_retry<R: ModelRunner>(
    runner: &R,
    call: &ModelCall,
) -> Result<ModelAttempts, RetryError<R::Error>> {
    let first = runner.run(call).map_err(RetryError::First)?;
    if first.outcome != ModelOutcome::Malformed {
        return Ok(ModelAttempts { first, retry: None });
    }
    match runner.run(call) {
        Ok(retry) => Ok(ModelAttempts {
            first,
            retry: Some(retry),
        }),
        Err(source) => Err(RetryError::Retry { first, source }),
    }
}
