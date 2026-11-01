//! `assay decide-only guard`'s outputs (03 §4a, #61 DO-10): `prs`, the PRs the redecide job's
//! matrix runs for, as a compact JSON array of PR numbers, and `trigger`, the event behind them.
//! A guard with nothing to decide writes `prs=[]` alone ([`DECIDE_ONLY_SKIP`]), and no job
//! needs it, so [`DecideOnlyGuardOutputs::from_needs`] refuses an empty set.

use std::fmt;
use std::num::NonZeroU64;

use assay_app::MAX_POLL_SELECTED;

use super::Read;
use crate::ci::{JobName, Needs, NeedsError, OutputName, canonical_positive};

/// The job whose outputs [`DecideOnlyGuardOutputs`] are: decide-only's guard job is `guard`.
const GUARD: JobName = JobName::Guard;

/// The most PRs one decide-only run decides: the poll's cap (D8).
pub const MAX_DECIDE_ONLY_PRS: usize = MAX_POLL_SELECTED;

/// What a decide-only guard with nothing to decide writes: `prs=[]`, and nothing else.
pub const DECIDE_ONLY_SKIP: [(OutputName, &str); 1] = [(OutputName::Prs, "[]")];

/// The event behind a decide-only run, as its guard reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecideTrigger {
    /// An `/assay` command comment (`issue_comment`).
    Comment,
    /// An `assay:*` label added or removed (`pull_request_target`).
    Label,
    /// The hourly poll (`schedule`).
    Schedule,
}

impl DecideTrigger {
    /// Every trigger.
    const ALL: [Self; 3] = [Self::Comment, Self::Label, Self::Schedule];

    /// The `trigger` output's value.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Label => "label",
            Self::Schedule => "schedule",
        }
    }
}

/// The PRs a decide-only run decides: at least one, at most [`MAX_DECIDE_ONLY_PRS`], each once,
/// in the order the guard chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecideOnlyPrs(Vec<NonZeroU64>);

/// Why a set of PRs was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrsError {
    /// There is none: a guard with nothing to decide writes [`DECIDE_ONLY_SKIP`].
    Empty,
    /// There are more than [`MAX_DECIDE_ONLY_PRS`].
    OverCap {
        /// How many there are.
        count: usize,
    },
    /// A PR is there twice.
    Repeated {
        /// The PR.
        pr: NonZeroU64,
    },
}

impl fmt::Display for PrsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a decide-only run decides at least one PR"),
            Self::OverCap { count } => write!(
                f,
                "{count} PRs are more than the {MAX_DECIDE_ONLY_PRS} a decide-only run decides"
            ),
            Self::Repeated { pr } => write!(f, "PR #{pr} is named twice"),
        }
    }
}

impl std::error::Error for PrsError {}

impl TryFrom<Vec<NonZeroU64>> for DecideOnlyPrs {
    type Error = PrsError;

    fn try_from(prs: Vec<NonZeroU64>) -> Result<Self, Self::Error> {
        if prs.is_empty() {
            return Err(PrsError::Empty);
        }
        if prs.len() > MAX_DECIDE_ONLY_PRS {
            return Err(PrsError::OverCap { count: prs.len() });
        }
        let mut seen = Vec::with_capacity(prs.len());
        for &pr in &prs {
            if seen.contains(&pr) {
                return Err(PrsError::Repeated { pr });
            }
            seen.push(pr);
        }
        Ok(Self(prs))
    }
}

impl DecideOnlyPrs {
    /// The one PR `pr`.
    #[must_use]
    pub fn one(pr: NonZeroU64) -> Self {
        Self(vec![pr])
    }

    /// The PRs, in order.
    #[must_use]
    pub fn as_slice(&self) -> &[NonZeroU64] {
        &self.0
    }

    /// The `prs` output's value: `[n,m,...]`, no space.
    fn encode(&self) -> String {
        let numbers: Vec<String> = self.0.iter().map(NonZeroU64::to_string).collect();
        format!("[{}]", numbers.join(","))
    }

    /// `text` read exactly as [`Self::encode`] writes it, or `None`.
    fn decode(text: &str) -> Option<Self> {
        let inner = text.strip_prefix('[')?.strip_suffix(']')?;
        let prs = inner
            .split(',')
            .map(canonical_positive)
            .collect::<Option<Vec<_>>>()?;
        Self::try_from(prs).ok()
    }
}

/// What a decide-only guard that found PRs to decide writes (03 §4a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecideOnlyGuardOutputs {
    prs: DecideOnlyPrs,
    trigger: DecideTrigger,
}

impl DecideOnlyGuardOutputs {
    /// The outputs for `prs`, triggered by `trigger`.
    #[must_use]
    pub const fn new(prs: DecideOnlyPrs, trigger: DecideTrigger) -> Self {
        Self { prs, trigger }
    }

    /// The PRs to decide.
    #[must_use]
    pub const fn prs(&self) -> &DecideOnlyPrs {
        &self.prs
    }

    /// The event behind them.
    #[must_use]
    pub const fn trigger(&self) -> DecideTrigger {
        self.trigger
    }

    /// The outputs, in [`OutputName::DECIDE_ONLY_GUARD`]'s order: `prs`, then `trigger`.
    #[must_use]
    pub fn outputs(&self) -> Vec<(OutputName, String)> {
        vec![
            (OutputName::Prs, self.prs.encode()),
            (OutputName::Trigger, self.trigger.word().to_owned()),
        ]
    }

    /// The outputs the decide-only guard wrote, read from `needs`.
    ///
    /// # Errors
    ///
    /// [`NeedsError::MissingJob`] without `guard`; [`NeedsError::NotSucceeded`] when it did
    /// not succeed; [`NeedsError::UnexpectedOutput`] for an output that is not decide-only
    /// guard's; [`NeedsError::MissingOutput`] for one absent or empty; and
    /// [`NeedsError::Output`] for `prs` not written exactly as the guard writes it (an empty
    /// set included: no job runs for it) or a `trigger` that is not decide-only's.
    pub fn from_needs(needs: &Needs) -> Result<Self, NeedsError> {
        let read = Read::succeeded(needs, GUARD, &OutputName::DECIDE_ONLY_GUARD)?;
        Ok(Self {
            prs: read.parsed(OutputName::Prs, DecideOnlyPrs::decode)?,
            trigger: read.parsed(OutputName::Trigger, |word| {
                DecideTrigger::ALL
                    .into_iter()
                    .find(|trigger| trigger.word() == word)
            })?,
        })
    }
}

#[cfg(test)]
mod tests;
