//! The deterministic scorer (04 §8): a reviewer finding matches a label when it is in the same
//! file, within [`LINE_WINDOW`] lines of the fixing hunk, and its class maps to the label's.
//! No LLM judge.

use std::num::NonZeroU64;

use assay_domain::Class;
use serde::Deserialize;
use serde_json::Value;

use super::case::Label;

/// How many lines before and after a fixing hunk a finding may start or end and still match.
pub const LINE_WINDOW: u64 = 10;

/// The most findings one model run may report; past it the run cannot be scored.
pub const MAX_RUN_FINDINGS: usize = 128;

/// Which finding classes count for which label class.
///
/// Every class counts for itself; [`ClassMap::with_alias`] lets one more finding class count
/// for a label class.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassMap {
    /// `(label class, finding class)` pairs that match beyond identity.
    aliases: Vec<(Class, Class)>,
}

impl ClassMap {
    /// Each class matches only itself.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            aliases: Vec::new(),
        }
    }

    /// This map, where a finding of class `finding` also matches a label of class `label`
    /// (one way only).
    #[must_use]
    pub fn with_alias(mut self, label: Class, finding: Class) -> Self {
        self.aliases.push((label, finding));
        self
    }

    /// Whether a finding of class `finding` counts for a label of class `label`.
    #[must_use]
    pub fn maps(&self, finding: Class, label: Class) -> bool {
        finding == label || self.aliases.contains(&(label, finding))
    }
}

/// The parts of a reviewer finding the scorer reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportedFinding {
    pub(crate) file: String,
    pub(crate) first: NonZeroU64,
    pub(crate) last: NonZeroU64,
    pub(crate) class: Class,
}

/// Why a model run's output could not be scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputProblem {
    /// The output is not an object with a `findings` array.
    NotADocument,
    /// More than [`MAX_RUN_FINDINGS`] findings.
    TooManyFindings,
    /// The finding at `index` has no string `file`, positive `line`, `end_line` (when present)
    /// at or after `line`, or known `class`.
    Finding {
        /// The finding's 0-based position.
        index: usize,
    },
}

/// The scored fields of one finding, as the model wrote them; every other field is ignored.
#[derive(Deserialize)]
struct FindingFields {
    file: String,
    line: NonZeroU64,
    #[serde(default)]
    end_line: Option<NonZeroU64>,
    class: Class,
}

/// The findings of a model run's structured output, refused past [`MAX_RUN_FINDINGS`].
pub(crate) fn read_findings(output: &Value) -> Result<Vec<ReportedFinding>, OutputProblem> {
    let list = output
        .get("findings")
        .and_then(Value::as_array)
        .ok_or(OutputProblem::NotADocument)?;
    if list.len() > MAX_RUN_FINDINGS {
        return Err(OutputProblem::TooManyFindings);
    }
    list.iter()
        .enumerate()
        .map(|(index, value)| {
            let refused = OutputProblem::Finding { index };
            let fields = FindingFields::deserialize(value).map_err(|_shape| refused)?;
            let end = fields.end_line.unwrap_or(fields.line);
            if end < fields.line {
                return Err(refused);
            }
            Ok(ReportedFinding {
                file: fields.file,
                first: fields.line,
                last: end,
                class: fields.class,
            })
        })
        .collect()
}

/// Whether `finding` matches `label` under `map`: same file, class mapped, and its lines
/// overlap the hunk widened by [`LINE_WINDOW`] on each side (stopping at line 1).
pub(crate) fn matches(label: &Label, finding: &ReportedFinding, map: &ClassMap) -> bool {
    let from = label.first().get().saturating_sub(LINE_WINDOW);
    let to = label.last().get().saturating_add(LINE_WINDOW);
    finding.file == label.file()
        && map.maps(finding.class, label.class())
        && finding.first.get() <= to
        && finding.last.get() >= from
}

#[cfg(test)]
mod tests;
