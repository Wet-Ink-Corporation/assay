//! The verifier's live reject rate (QA-12, 03 §6), computed from `FindingVerified` events.

use std::collections::BTreeSet;

use assay_domain::{Event, EventPayload, RunUid, VerifierVerdict};

/// The fewest runs with a verdict over which the reject rate is judged (QA-12).
pub const REJECT_RATE_MIN_RUNS: usize = 20;

/// The lowest reject rate that raises no alert, in percent (QA-12).
const BAND_LOW_PERCENT: u128 = 10;

/// The highest reject rate that raises no alert, in percent (QA-12).
const BAND_HIGH_PERCENT: u128 = 60;

/// How the live reject rate stands against QA-12's band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectRate {
    /// Fewer than [`REJECT_RATE_MIN_RUNS`] runs had a verdict: no alert either way.
    TooFewRuns,
    /// From 10 % to 60 % of verdicts were `refuted`, both ends included.
    Within,
    /// Under 10 %: the verifier may be confirming everything.
    Below,
    /// Over 60 %: the verifier may be refuting true findings, or the reviewers are noisy.
    Above,
}

/// The verdicts of `FindingVerified` events and the runs they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerdictTally {
    confirmed: u64,
    refuted: u64,
    unclear: u64,
    runs: usize,
}

impl VerdictTally {
    /// Counts the `FindingVerified` events among `events` by verdict, and the distinct runs
    /// (`run_uid`) that emitted one. Other kinds are skipped.
    #[must_use]
    pub fn of<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut tally = Self::default();
        let mut runs: BTreeSet<RunUid> = BTreeSet::new();
        for event in events {
            let EventPayload::FindingVerified { verdict } = event.payload else {
                continue;
            };
            runs.insert(event.run_uid);
            let count = match verdict {
                VerifierVerdict::Confirmed => &mut tally.confirmed,
                VerifierVerdict::Refuted => &mut tally.refuted,
                VerifierVerdict::Unclear => &mut tally.unclear,
            };
            *count = count.saturating_add(1);
        }
        tally.runs = runs.len();
        tally
    }

    /// `confirmed` verdicts.
    #[must_use]
    pub const fn confirmed(&self) -> u64 {
        self.confirmed
    }

    /// `refuted` verdicts: the rejections.
    #[must_use]
    pub const fn refuted(&self) -> u64 {
        self.refuted
    }

    /// `unclear` verdicts.
    #[must_use]
    pub const fn unclear(&self) -> u64 {
        self.unclear
    }

    /// The distinct runs that emitted a verdict.
    #[must_use]
    pub const fn runs(&self) -> usize {
        self.runs
    }

    /// The reject rate, `refuted` over every verdict, against QA-12's band of 10 % to 60 % over
    /// at least [`REJECT_RATE_MIN_RUNS`] runs. Exact: compared in integers, never as a float.
    #[must_use]
    pub fn reject_rate(&self) -> RejectRate {
        if self.runs < REJECT_RATE_MIN_RUNS {
            return RejectRate::TooFewRuns;
        }
        let verdicts =
            u128::from(self.confirmed) + u128::from(self.refuted) + u128::from(self.unclear);
        let refuted = u128::from(self.refuted) * 100;
        if refuted < BAND_LOW_PERCENT * verdicts {
            RejectRate::Below
        } else if refuted > BAND_HIGH_PERCENT * verdicts {
            RejectRate::Above
        } else {
            RejectRate::Within
        }
    }
}
