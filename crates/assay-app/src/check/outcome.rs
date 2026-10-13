//! How a local gate settled, as the report writes it: `pass`, `fail`, `neutral` or
//! `gate-error:<detail>` (#219 F11: a closed set, not free text).

use std::fmt;
use std::str::FromStr;

use assay_domain::{GateConclusion, GateOutcome, StepAttribution};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::gate::Conclusion;

/// What precedes a gate error's detail.
const GATE_ERROR: &str = "gate-error:";

/// The longest gate-error detail read back, in bytes.
const MAX_DETAIL_BYTES: usize = 64;

/// How a local gate settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOutcome {
    /// It concluded `pass`.
    Pass,
    /// It concluded `fail`.
    Fail,
    /// It concluded `neutral`.
    Neutral,
    /// It did not count: `gate-error:<detail>`.
    GateError(GateErrorText),
}

/// A gate error as the report writes it, `gate-error:<detail>`, its detail in assay's words
/// (`not-run`, `evidence-missing`, `exit-3`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateErrorText(String);

impl GateErrorText {
    /// The error whose detail is `detail`, one of assay's own.
    pub(super) fn of(detail: &str) -> Self {
        Self([GATE_ERROR, detail].concat())
    }

    /// The whole text, `gate-error:<detail>`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The detail after `gate-error:`.
    #[must_use]
    pub fn detail(&self) -> &str {
        self.0.strip_prefix(GATE_ERROR).unwrap_or(&self.0)
    }
}

impl LocalOutcome {
    /// The outcome as the report writes it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Neutral => "neutral",
            Self::GateError(text) => text.as_str(),
        }
    }

    /// The outcome of a gate that concluded `conclusion`.
    pub(super) const fn concluded(conclusion: Conclusion) -> Self {
        match conclusion {
            Conclusion::Pass => Self::Pass,
            Conclusion::Fail => Self::Fail,
            Conclusion::Neutral => Self::Neutral,
        }
    }

    /// The outcome `decide` reads.
    pub(super) fn to_domain(&self) -> GateOutcome {
        let conclusion = match self {
            Self::Pass => GateConclusion::Pass,
            Self::Fail => GateConclusion::Fail,
            Self::Neutral => GateConclusion::Neutral,
            Self::GateError(text) => {
                return GateOutcome::Error {
                    detail: text.detail().to_owned(),
                };
            }
        };
        GateOutcome::Ran {
            conclusion,
            steps: StepAttribution::Unknown,
        }
    }

    /// The conclusion the gate's check would have: a gate error fails, as `publish` writes it.
    pub(crate) const fn check_conclusion(&self) -> GateCheck {
        match self {
            Self::Pass => GateCheck::Success,
            Self::Neutral => GateCheck::Neutral,
            Self::Fail | Self::GateError(_) => GateCheck::Failure,
        }
    }
}

/// A gate's check conclusion, ordered from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GateCheck {
    /// `success`.
    Success,
    /// `neutral`.
    Neutral,
    /// `failure`.
    Failure,
}

impl GateCheck {
    /// GitHub's word for it.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Neutral => "neutral",
            Self::Failure => "failure",
        }
    }
}

impl fmt::Display for LocalOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for LocalOutcome {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// A report outcome that is not `pass`, `fail`, `neutral` or `gate-error:<detail>` with a
/// detail of 1 to 64 of `a-z`, `0-9` and `-`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a local gate outcome")]
pub struct InvalidOutcome;

impl FromStr for LocalOutcome {
    type Err = InvalidOutcome;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "pass" => Ok(Self::Pass),
            "fail" => Ok(Self::Fail),
            "neutral" => Ok(Self::Neutral),
            _ => {
                let detail = text.strip_prefix(GATE_ERROR).ok_or(InvalidOutcome)?;
                let reads = (1..=MAX_DETAIL_BYTES).contains(&detail.len())
                    && detail.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    });
                if reads {
                    Ok(Self::GateError(GateErrorText::of(detail)))
                } else {
                    Err(InvalidOutcome)
                }
            }
        }
    }
}

impl Serialize for LocalOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LocalOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{GateErrorText, InvalidOutcome, LocalOutcome};

    #[test]
    fn every_outcome_reads_back_from_its_text() {
        let outcomes = [
            LocalOutcome::Pass,
            LocalOutcome::Fail,
            LocalOutcome::Neutral,
            LocalOutcome::GateError(GateErrorText::of("exit--1")),
        ];

        let read: Vec<Result<LocalOutcome, InvalidOutcome>> = outcomes
            .iter()
            .map(|outcome| outcome.as_str().parse())
            .collect();

        assert_eq!(read, outcomes.map(Ok));
    }

    #[test]
    fn a_gate_errors_detail_is_read_up_to_64_bytes_of_its_alphabet() {
        let longest = format!("gate-error:{}", "a".repeat(64));
        let over = format!("gate-error:{}", "a".repeat(65));

        let read = [
            longest.as_str(),
            over.as_str(),
            "gate-error:",
            "gate-error:Not-Run",
            "gate-error:not run",
            "PASS",
            "",
        ]
        .map(|text| {
            text.parse::<LocalOutcome>()
                .map(|outcome| outcome.to_string())
        });

        assert_eq!(
            read,
            [
                Ok(longest),
                Err(InvalidOutcome),
                Err(InvalidOutcome),
                Err(InvalidOutcome),
                Err(InvalidOutcome),
                Err(InvalidOutcome),
                Err(InvalidOutcome),
            ]
        );
    }

    #[test]
    fn a_gate_error_fails_its_check_like_a_failing_gate() {
        let conclusions = [
            LocalOutcome::Pass,
            LocalOutcome::Fail,
            LocalOutcome::Neutral,
            LocalOutcome::GateError(GateErrorText::of("not-run")),
        ]
        .map(|outcome| outcome.check_conclusion().code());

        assert_eq!(conclusions, ["success", "failure", "neutral", "failure"]);
    }
}
