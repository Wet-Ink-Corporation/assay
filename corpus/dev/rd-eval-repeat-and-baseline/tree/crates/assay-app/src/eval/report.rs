//! What an evaluation found, per case and run, and 04 §8's accept rule over two of them.

use std::collections::BTreeMap;

use assay_domain::{Aeu, ContentHash};
use thiserror::Error;

use super::EVAL_RUNS;
use super::case::{AuthoringModel, CaseSource};
use super::corpus::Split;
use super::ledger::EvalMonth;
use super::score::OutputProblem;

/// The normal quantile of the Wilson score interval: 1.96, for 95% two-sided.
pub const WILSON_Z: f64 = 1.96;

/// How one model run of one case scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunScore {
    /// A defect case: some finding matched some label.
    Detected,
    /// A defect case: no finding matched any label.
    Missed,
    /// A true-negative case: no finding.
    Quiet,
    /// A true-negative case: this many findings, all noise.
    Noisy {
        /// How many findings the run reported.
        findings: usize,
    },
    /// The run could not be scored. It counts against the change: a miss on a defect case and
    /// noise on a true-negative case.
    Unscorable(Unscorable),
}

/// Why a run could not be scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unscorable {
    /// The runner could not make or supervise the call.
    CallFailed,
    /// The session was not confined to `Read`, `Grep` and `Glob`, or its init was unreadable.
    Unconfined,
    /// The output was malformed, also on the retry.
    Malformed,
    /// The call stopped at its budget.
    BudgetExhausted,
    /// The call stopped at its turn limit.
    TurnsExhausted,
    /// The call timed out.
    Timeout,
    /// The subscription's usage limit refused the call.
    QuotaExhausted,
    /// The structured output could not be read.
    Output(OutputProblem),
}

/// Whether a case has something to find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    /// It holds at least one label.
    Defect,
    /// It holds none: every finding is noise.
    TrueNegative,
}

/// One case's [`EVAL_RUNS`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseOutcome {
    pub(crate) id: String,
    pub(crate) authoring_model: AuthoringModel,
    pub(crate) source: CaseSource,
    pub(crate) kind: CaseKind,
    pub(crate) runs: [RunScore; EVAL_RUNS],
}

impl CaseOutcome {
    /// The case's id (its directory name).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Who wrote the change.
    #[must_use]
    pub const fn authoring_model(&self) -> &AuthoringModel {
        &self.authoring_model
    }

    /// Where the case came from.
    #[must_use]
    pub const fn source(&self) -> CaseSource {
        self.source
    }

    /// Whether the case has something to find.
    #[must_use]
    pub const fn kind(&self) -> CaseKind {
        self.kind
    }

    /// Each run's score, in run order.
    #[must_use]
    pub const fn runs(&self) -> &[RunScore; EVAL_RUNS] {
        &self.runs
    }

    /// How many runs detected the defect (0 for a true-negative case).
    #[must_use]
    pub fn detected(&self) -> u32 {
        self.count(|run| matches!(run, RunScore::Detected))
    }

    /// How many runs were noise on a true-negative case (unscorable runs included); 0 for a
    /// defect case.
    #[must_use]
    pub fn noisy(&self) -> u32 {
        if self.kind == CaseKind::Defect {
            return 0;
        }
        self.count(|run| matches!(run, RunScore::Noisy { .. } | RunScore::Unscorable(_)))
    }

    /// How many runs the case had: [`EVAL_RUNS`].
    fn run_count(&self) -> u32 {
        self.count(|_run| true)
    }

    fn count(&self, counted: impl Fn(&RunScore) -> bool) -> u32 {
        self.runs
            .iter()
            .filter(|run| counted(run))
            .fold(0, |count, _run| count.saturating_add(1))
    }
}

/// `hits` out of `n`, reported with `n` (04 §8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Proportion {
    hits: u32,
    n: u32,
}

impl Proportion {
    /// How many counted.
    #[must_use]
    pub const fn hits(self) -> u32 {
        self.hits
    }

    /// Out of how many.
    #[must_use]
    pub const fn n(self) -> u32 {
        self.n
    }

    /// `hits / n`; `None` when `n` is 0.
    #[must_use]
    pub fn rate(self) -> Option<f64> {
        (self.n > 0).then(|| f64::from(self.hits) / f64::from(self.n))
    }

    /// The upper end of the 95% Wilson score interval ([`WILSON_Z`]); `None` when `n` is 0.
    #[must_use]
    pub fn wilson_upper(self) -> Option<f64> {
        let rate = self.rate()?;
        let n = f64::from(self.n);
        let z2 = WILSON_Z * WILSON_Z;
        let centre = rate + z2 / (2.0 * n);
        let spread = WILSON_Z * (rate * (1.0 - rate) / n + z2 / (4.0 * n * n)).sqrt();
        Some(((centre + spread) / (1.0 + z2 / n)).min(1.0))
    }

    fn add(self, hits: u32, n: u32) -> Self {
        Self {
            hits: self.hits.saturating_add(hits),
            n: self.n.saturating_add(n),
        }
    }
}

/// One evaluation of one split under one prompt bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalReport {
    pub(crate) split: Split,
    pub(crate) bundle: ContentHash,
    pub(crate) month: EvalMonth,
    pub(crate) spent: Aeu,
    pub(crate) cases: Vec<CaseOutcome>,
}

impl EvalReport {
    /// The split evaluated.
    #[must_use]
    pub const fn split(&self) -> Split {
        self.split
    }

    /// The hash of the prompt bundle evaluated.
    #[must_use]
    pub const fn bundle(&self) -> ContentHash {
        self.bundle
    }

    /// The eval ledger month charged.
    #[must_use]
    pub const fn month(&self) -> EvalMonth {
        self.month
    }

    /// What this evaluation charged to the eval ledger.
    #[must_use]
    pub const fn spent(&self) -> Aeu {
        self.spent
    }

    /// Every case, in id order.
    #[must_use]
    pub fn cases(&self) -> &[CaseOutcome] {
        &self.cases
    }

    /// Detected runs out of defect-case runs, per authoring model, so self-review recall is
    /// reported apart from human-authored recall (04 §8).
    #[must_use]
    pub fn recall_by_author(&self) -> BTreeMap<&AuthoringModel, Proportion> {
        let mut recall: BTreeMap<&AuthoringModel, Proportion> = BTreeMap::new();
        for case in self.defects() {
            let entry = recall.entry(&case.authoring_model).or_default();
            *entry = entry.add(case.detected(), case.run_count());
        }
        recall
    }

    /// Noisy runs out of true-negative-case runs.
    #[must_use]
    pub fn noise(&self) -> Proportion {
        self.cases
            .iter()
            .filter(|case| case.kind == CaseKind::TrueNegative)
            .fold(Proportion::default(), |noise, case| {
                noise.add(case.noisy(), case.run_count())
            })
    }

    fn defects(&self) -> impl Iterator<Item = &CaseOutcome> {
        self.cases
            .iter()
            .filter(|case| case.kind == CaseKind::Defect)
    }
}

/// How the true-negative noise rate moved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoiseCheck {
    /// The candidate's rate is at most `bound`, the baseline's Wilson upper bound.
    Within {
        /// The baseline's Wilson upper bound.
        bound: f64,
    },
    /// The candidate's rate is above `bound`.
    Rose {
        /// The baseline's Wilson upper bound.
        bound: f64,
    },
    /// The split has no true-negative case, so noise cannot be judged and the change is not
    /// accepted.
    NoTrueNegatives,
}

/// The accept rule's result for a candidate against a baseline (04 §8).
#[derive(Debug, Clone, PartialEq)]
pub struct Acceptance {
    flips: Vec<String>,
    baseline_noise: Proportion,
    candidate_noise: Proportion,
    noise: NoiseCheck,
}

impl Acceptance {
    /// Whether the change is accepted: no case flipped and noise stayed within its margin.
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.flips.is_empty() && matches!(self.noise, NoiseCheck::Within { .. })
    }

    /// The defect cases detected in at least 2 of 3 baseline runs and at most 1 of 3
    /// candidate runs, in id order.
    #[must_use]
    pub fn flips(&self) -> &[String] {
        &self.flips
    }

    /// The baseline's noise, with `n`.
    #[must_use]
    pub const fn baseline_noise(&self) -> Proportion {
        self.baseline_noise
    }

    /// The candidate's noise, with `n`.
    #[must_use]
    pub const fn candidate_noise(&self) -> Proportion {
        self.candidate_noise
    }

    /// How noise moved.
    #[must_use]
    pub const fn noise(&self) -> NoiseCheck {
        self.noise
    }
}

/// Why two reports cannot be compared.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompareError {
    /// They evaluated different splits.
    #[error("the reports evaluated different splits")]
    SplitDiffers,
    /// They evaluated different cases; `case` is the first id (in order) only one holds.
    #[error("the reports evaluated different cases (first difference: {case:?})")]
    CaseSetDiffers {
        /// The first id only one report holds.
        case: String,
    },
}

/// Detections a baseline case needs to count as detected: at least 2 of 3.
const DETECTED_AT_LEAST: u32 = 2;

/// Detections at or below which a candidate case counts as missed: at most 1 of 3.
const MISSED_AT_MOST: u32 = 1;

/// Applies 04 §8's accept rule to `candidate` against `baseline`.
///
/// The change is accepted when no defect case flips from at least 2 of 3 detected (baseline)
/// to at most 1 of 3 (candidate), and the candidate's true-negative noise rate is at most the
/// baseline's 95% Wilson upper bound.
///
/// # Errors
/// [`CompareError`] when the reports evaluated different splits or cases.
pub fn compare(baseline: &EvalReport, candidate: &EvalReport) -> Result<Acceptance, CompareError> {
    if baseline.split != candidate.split {
        return Err(CompareError::SplitDiffers);
    }
    let mut flips = Vec::new();
    let mut pairs = baseline.cases.iter().zip(&candidate.cases);
    if let Some((before, after)) = pairs.find(|(before, after)| before.id != after.id) {
        let case = before.id.as_str().min(after.id.as_str());
        return Err(CompareError::CaseSetDiffers {
            case: case.to_owned(),
        });
    }
    if let Some(extra) = extra_case(baseline, candidate) {
        return Err(CompareError::CaseSetDiffers {
            case: extra.to_owned(),
        });
    }
    for (before, after) in baseline.cases.iter().zip(&candidate.cases) {
        let flipped = before.kind == CaseKind::Defect
            && before.detected() >= DETECTED_AT_LEAST
            && after.detected() <= MISSED_AT_MOST;
        if flipped {
            // The acceptance outlives neither report but owns its list of ids.
            flips.push(after.id.clone());
        }
    }
    let baseline_noise = baseline.noise();
    let candidate_noise = candidate.noise();
    let noise = match (baseline_noise.wilson_upper(), candidate_noise.rate()) {
        (Some(bound), Some(rate)) if rate <= bound => NoiseCheck::Within { bound },
        (Some(bound), Some(_)) => NoiseCheck::Rose { bound },
        _ => NoiseCheck::NoTrueNegatives,
    };
    Ok(Acceptance {
        flips,
        baseline_noise,
        candidate_noise,
        noise,
    })
}

/// The first case one report holds past the other's last, when their lengths differ.
fn extra_case<'a>(baseline: &'a EvalReport, candidate: &'a EvalReport) -> Option<&'a str> {
    let shorter = baseline.cases.len().min(candidate.cases.len());
    baseline
        .cases
        .get(shorter)
        .or_else(|| candidate.cases.get(shorter))
        .map(|case| case.id.as_str())
}

#[cfg(test)]
mod tests;
