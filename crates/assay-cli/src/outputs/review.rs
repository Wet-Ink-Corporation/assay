//! The review job's outputs (03 §4a, #60 ST-9, D11 A): `stage`, how far the review got;
//! `cost_aeu`, what it spent, in the ledger's form; and `documents`, none until the real review
//! (M2) writes some.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use assay_app::parse_aeu;
use assay_domain::Aeu;

use super::{only_own, written};
use crate::ci::{JobName, JobResult, Needs, NeedsError, OutputName};

/// The job whose outputs [`ReviewEnd`] reads.
const REVIEW: JobName = JobName::Review;

/// The `documents` value of a review that reported none.
const NO_DOCUMENTS: &str = "[]";

/// The review job's outputs while it is a placeholder (#60 ST-9, D11 A).
///
/// In [`OutputName::REVIEW`]'s order: the review did not run (`stage=not-run`, so `decide`
/// applies the LLM-unavailable policy), spent nothing and reported no document. The real
/// review (M2) replaces it.
pub const REVIEW_PLACEHOLDER: [(OutputName, &str); 3] = [
    (OutputName::Stage, ReviewStage::NotRun.as_str()),
    (OutputName::CostAeu, "0"),
    (OutputName::Documents, NO_DOCUMENTS),
];

/// How far the review got, as its `stage` output says. Only the placeholder's stage exists
/// until the real review (M2) adds its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStage {
    /// `not-run`: the review did not run.
    NotRun,
}

impl ReviewStage {
    /// The stage's word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRun => "not-run",
        }
    }
}

/// A `stage` output that is not a stage's word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownReviewStage;

impl fmt::Display for UnknownReviewStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a review stage")
    }
}

impl Error for UnknownReviewStage {}

impl FromStr for ReviewStage {
    type Err = UnknownReviewStage;

    /// The stage whose word is exactly `text`.
    fn from_str(text: &str) -> Result<Self, UnknownReviewStage> {
        [Self::NotRun]
            .into_iter()
            .find(|stage| stage.as_str() == text)
            .ok_or(UnknownReviewStage)
    }
}

/// What a review that succeeded reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewReport {
    /// How far it got.
    pub stage: ReviewStage,
    /// What it spent.
    pub cost: Aeu,
}

/// How the review job ended, as its needs entry says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEnd {
    /// It succeeded and reported its outputs.
    Reported(ReviewReport),
    /// It failed: its outputs are not read.
    Failed,
    /// It was cancelled: its outputs are not read.
    Cancelled,
    /// It was skipped: its outputs are not read.
    Skipped,
}

impl ReviewEnd {
    /// How the review ended, read from `needs`; its outputs are read only when it succeeded.
    ///
    /// # Errors
    ///
    /// [`NeedsError::MissingJob`] without `review`; for a review that succeeded,
    /// [`NeedsError::UnexpectedOutput`] for an output that is not the review's,
    /// [`NeedsError::MissingOutput`] for one absent or empty, and [`NeedsError::Output`] for a
    /// `stage` that is not a stage's word, a `cost_aeu` not in the ledger's form
    /// ([`parse_aeu`]) or `documents` other than exactly `[]`.
    pub fn from_needs(needs: &Needs) -> Result<Self, NeedsError> {
        let review = needs
            .job(REVIEW)
            .ok_or(NeedsError::MissingJob { job: REVIEW })?;
        match review.result() {
            JobResult::Success => {}
            JobResult::Failure => return Ok(Self::Failed),
            JobResult::Cancelled => return Ok(Self::Cancelled),
            JobResult::Skipped => return Ok(Self::Skipped),
        }
        only_own(review, REVIEW, &OutputName::REVIEW)?;
        let required =
            |name| written(review, name).ok_or(NeedsError::MissingOutput { job: REVIEW, name });
        let invalid = |name| NeedsError::Output { job: REVIEW, name };
        let stage = required(OutputName::Stage)?
            .parse()
            .map_err(|UnknownReviewStage| invalid(OutputName::Stage))?;
        let cost = parse_aeu(required(OutputName::CostAeu)?)
            .ok_or_else(|| invalid(OutputName::CostAeu))?;
        if required(OutputName::Documents)? != NO_DOCUMENTS {
            return Err(invalid(OutputName::Documents));
        }
        Ok(Self::Reported(ReviewReport { stage, cost }))
    }
}

#[cfg(test)]
mod tests;
