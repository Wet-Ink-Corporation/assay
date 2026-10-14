//! Which findings the verifier judges, in what order, and what it sees of each (04 §3, §4 step 8,
//! §7).

use std::cmp::Reverse;
use std::num::NonZeroU64;

use assay_domain::{Class, Confidence, Severity};
use serde_json::json;

use super::VerifierTier;
use crate::model_runner::{InvalidPrompt, Prompt};
use crate::prompt_bundle::PromptBundle;
use crate::rereview::CarriedFinding;
use crate::validation::{FindingRef, ValidFinding};

/// Which claim a verifier result is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimRef {
    /// A finding this run's reviewers raised, as validation named it.
    Raised(FindingRef),
    /// A finding carried from an earlier run: its position in [`VerifyRequest::carried`].
    ///
    /// [`VerifyRequest::carried`]: super::VerifyRequest::carried
    Carried(usize),
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
    claim: Claim<'a>,
}

/// What the verifier sees of a candidate (04 §3 "Blindness"), and how it is ranked.
#[derive(Debug)]
struct Claim<'a> {
    file: &'a str,
    line: NonZeroU64,
    class: Class,
    title: &'a str,
    trigger: Option<&'a str>,
    severity: Severity,
    confidence: Option<Confidence>,
}

impl Candidate<'_> {
    /// The verifier's user prompt: the claim as one JSON object of exactly the five fields the
    /// verifier may see, a blank line, then the instruction (04 §3 "Blindness"): for a carried
    /// finding the one asking whether the defect is still present at head (04 §7).
    pub(super) fn prompt(&self, bundle: &PromptBundle) -> Result<Prompt, InvalidPrompt> {
        let Claim {
            file,
            line,
            class,
            title,
            trigger,
            ..
        } = self.claim;
        let claim = json!({
            "file": file,
            "line": line,
            "class": class.code(),
            "claim": title,
            "trigger": trigger,
        });
        let instruction = match self.slot {
            Slot::Raised(_) => bundle.verifier_instruction(),
            Slot::Carried(_) => bundle.carried_verifier_instruction(),
        };
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
            at: ClaimRef::Carried(index),
            slot: Slot::Carried(index),
            claim: Claim {
                file: found.file(),
                line: found.line(),
                class: found.class(),
                title: found.title(),
                trigger: found.trigger(),
                severity: found.severity(),
                confidence: None,
            },
        })
        .collect();
    // Stable sorts: equal keys keep their input order.
    carried.sort_by_key(|candidate| Reverse(candidate.claim.severity));
    let mut raised: Vec<Candidate<'a>> = raised
        .iter()
        .enumerate()
        .filter(|(_, found)| is_candidate(found.finding.severity))
        .map(|(index, found)| Candidate {
            at: ClaimRef::Raised(found.at),
            slot: Slot::Raised(index),
            claim: Claim {
                file: &found.file_at_head,
                line: found.finding.line,
                class: found.finding.class,
                title: &found.finding.title,
                trigger: found.finding.trigger.as_deref(),
                severity: found.finding.severity,
                confidence: found.finding.confidence,
            },
        })
        .collect();
    raised.sort_by_key(|candidate| {
        (
            Reverse(candidate.claim.severity),
            confidence_rank(candidate.claim.confidence),
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
