//! The eval ledger (04 §8): eval spend is drawn from its own monthly cap, never from the
//! production daily cap, so an evaluation cannot starve PR review.

use std::fmt;

use assay_domain::Aeu;

use crate::calendar::CalendarDate;

/// The eval ledger's monthly cap: 150 AEU.
pub const EVAL_MONTHLY_CAP: Aeu = Aeu::from_micros(150_000_000);

/// A calendar month (UTC), the eval ledger's period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvalMonth {
    year: u32,
    month: u32,
}

impl EvalMonth {
    /// The month `day` falls in.
    #[must_use]
    pub const fn of(day: CalendarDate) -> Self {
        let (year, month) = day.year_month();
        Self { year, month }
    }
}

impl fmt::Display for EvalMonth {
    /// `YYYY-MM`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

/// The eval ledger port: what evaluations have spent per month. Implemented by an adapter
/// apart from the production ledger; no production cap is ever read or charged through it.
pub trait EvalLedger {
    /// Why the ledger could not be read or written.
    type Error: std::error::Error;

    /// What evaluations have spent in `month`.
    ///
    /// # Errors
    /// Returns [`Self::Error`] when the ledger cannot be read.
    fn spent(&self, month: EvalMonth) -> Result<Aeu, Self::Error>;

    /// Records `cost` as spent in `month`.
    ///
    /// # Errors
    /// Returns [`Self::Error`] when the charge could not be recorded.
    fn charge(&mut self, month: EvalMonth, cost: Aeu) -> Result<(), Self::Error>;
}
