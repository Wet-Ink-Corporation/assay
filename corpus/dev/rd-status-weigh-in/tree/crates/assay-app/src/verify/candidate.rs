//! Which findings the verifier judges, in what order, and what it sees of each (04 §3, §4 step 8,
//! §7).

use std::cmp::Reverse;

use assay_domain::{Confidence, Finding, Severity};
use serde_json::json;

use super::VerifierTier;
use crate::model_runner::{InvalidPrompt, Prompt};
use crate::validation::{FindingRef, ValidFinding};

/// A finding open from an earlier run of the PR, to be judged again at head (04 §7).
///
/// It goes to the verifier whatever its severity, ahead of this run's candidates, and shares
/// the tier's calls with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedFinding {
    /// The finding as it was last published, its `line` the line at head the caller anchored it
    /// to; the verifier sees only its `line`, `class`, `title` and `trigger`.
    pub finding: Finding,
    /// Its file's path at head.
    pub file_at_head: String,
}

/// Which claim a verifier result is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimRef {
    /// A finding this run's reviewers raised, as validation named it.
    Raised(FindingRef),
    /// A finding carried from an earlier run: its position in [`VerifyRequest::carried`].
    ///
    /// [`VerifyRequest::carried`]: super::VerifyRequest::carried
    Carried {
        /// Its position among the carried findings given.
        index: usize,
    },
}

/// Where a candidate's result is recorded: its position among the raised or carried findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    Raised(usize),
    Carried(usize),
}

/// A finding picked for a call.
#[derive(Debug)]
pub(super) struct Candidate<'a> {
    pub(super) at: ClaimRef,
    pub(super) slot: Slot,
    file_at_head: &'a str,
    finding: &'a Finding,
}

impl Candidate<'_> {
    /// The verifier's user prompt: the claim as one JSON object of exactly the five fields the
    /// verifier may see, a blank line, then the instruction (04 §3 "Blindness").
    pub(super) fn prompt(&self, instruction: &str) -> Result<Prompt, InvalidPrompt> {
        let claim = json!({
            "file": self.file_at_head,
            "line": self.finding.line,
            "class": self.finding.class.code(),
            "claim": self.finding.title,
            "trigger": self.finding.trigger,
        });
        Prompt::new(format!("{claim}\n\n{instruction}"))
    }
}

/// Whether a raised finding is a candidate: at least `major` (04 §3).
pub(super) fn is_candidate(severity: Severity) -> bool {
    severity >= Severity::Major
}

/// The candidates the tier verifies, in the order they are judged: every carried finding, by
/// severity then in order; then the raised candidates, blockers first, then by confidence,
/// then in order; at most [`VerifierTier::calls`] in all.
pub(super) fn candidates<'a>(
    tier: VerifierTier,
    carried: &'a [CarriedFinding],
    raised: &'a [ValidFinding],
) -> Vec<Candidate<'a>> {
    let mut carried: Vec<Candidate<'a>> = carried
        .iter()
        .enumerate()
        .map(|(index, found)| Candidate {
            at: ClaimRef::Carried { index },
            slot: Slot::Carried(index),
            file_at_head: &found.file_at_head,
            finding: &found.finding,
        })
        .collect();
    // Stable sorts: equal keys keep their input order.
    carried.sort_by_key(|candidate| Reverse(candidate.finding.severity));
    let mut raised: Vec<Candidate<'a>> = raised
        .iter()
        .enumerate()
        .filter(|(_, found)| is_candidate(found.finding.severity))
        .map(|(index, found)| Candidate {
            at: ClaimRef::Raised(found.at),
            slot: Slot::Raised(index),
            file_at_head: &found.file_at_head,
            finding: &found.finding,
        })
        .collect();
    raised.sort_by_key(|candidate| {
        (
            Reverse(candidate.finding.severity),
            confidence_rank(candidate.finding.confidence),
        )
    });
    carried.extend(raised);
    carried.truncate(tier.calls());
    carried
}

/// Higher confidence first; a finding that states none goes last.
const fn confidence_rank(confidence: Option<Confidence>) -> u8 {
    match confidence {
        Some(Confidence::High) => 0,
        Some(Confidence::Medium) => 1,
        Some(Confidence::Low) => 2,
        None => 3,
    }
}
