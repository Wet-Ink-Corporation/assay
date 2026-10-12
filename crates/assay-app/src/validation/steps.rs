//! Steps 2 to 6 of 04 §4 for one finding, in order; a dropping step ends the finding.

use std::num::NonZeroU64;

use assay_domain::{
    DismissalMatch, Disposition, Finding, FindingKind, Fingerprint, IntroducedBy, Severity, Side,
};

use super::change::Change;
use super::{
    FindingRef, Inputs, Placement, Transition, ValidFinding, Validated, evidence, screen, symbol,
};

/// How many lines apart a reviewer finding may be from a gate finding of the same class in the
/// same file and still duplicate it (04 §1).
const GATE_DUPLICATE_LINES: u64 = 3;

/// Validates one schema-valid finding, appending its transitions to `validated` and the finding
/// itself if it survives.
pub(super) fn check(
    inputs: &Inputs<'_>,
    at: FindingRef,
    finding: Finding,
    validated: &mut Validated,
) {
    let mut steps = Steps {
        inputs,
        at,
        transitions: Vec::new(),
    };
    let outcome = steps.apply(finding);
    validated.transitions.append(&mut steps.transitions);
    match outcome {
        Ok(kept) => validated.findings.push(kept),
        Err(code) => validated.transitions.push(Transition::of(at, code)),
    }
}

/// One finding on its way through the steps, with the transitions it has had so far.
struct Steps<'a> {
    inputs: &'a Inputs<'a>,
    at: FindingRef,
    transitions: Vec<Transition>,
}

/// What step 2 established about a finding.
struct Anchored {
    file_at_head: String,
    enclosing_symbol: String,
    occurrence_index: u32,
    fingerprint: Fingerprint,
    in_diff: bool,
}

impl Steps<'_> {
    /// Steps 2 to 6; `Err` is the disposition that drops the finding.
    fn apply(&mut self, mut finding: Finding) -> Result<ValidFinding, Disposition> {
        shape(&finding)?;
        let anchored = anchor(self.inputs.change, &mut finding)?;
        let placement = self.place(&mut finding, anchored.in_diff);
        self.require_trigger(&mut finding);
        if free_text(&finding).any(|text| screen::screen(text, self.inputs.repository).is_some()) {
            return Err(Disposition::InjectionSuspect);
        }
        let previously_dismissed = self.suppress(&finding, &anchored)?;
        Ok(ValidFinding {
            at: self.at,
            finding,
            file_at_head: anchored.file_at_head,
            enclosing_symbol: anchored.enclosing_symbol,
            occurrence_index: anchored.occurrence_index,
            fingerprint: anchored.fingerprint,
            placement,
            previously_dismissed,
        })
    }

    fn record(&mut self, transition: Transition) {
        self.transitions.push(transition);
    }

    /// Step 3: inline only on a hunk line, else moved to the summary; `pre_existing` honoured
    /// only outside the diff and not for an interaction.
    fn place(&mut self, finding: &mut Finding, in_diff: bool) -> Placement {
        let placement = if in_diff {
            Placement::Inline
        } else {
            self.record(Transition::of(self.at, Disposition::LineNotInDiffMoved));
            Placement::Summary
        };
        if finding.pre_existing {
            let interaction = finding.introduced_by == Some(IntroducedBy::Interaction);
            if in_diff || interaction {
                finding.pre_existing = false;
            } else {
                self.record(Transition::of(self.at, Disposition::PreExisting));
            }
        }
        placement
    }

    /// Step 4: from `major` up a trigger with content is required, else the finding is capped
    /// at `minor`.
    fn require_trigger(&mut self, finding: &mut Finding) {
        let triggered = finding
            .trigger
            .as_deref()
            .is_some_and(|trigger| !trigger.trim().is_empty());
        if finding.severity >= Severity::Major && !triggered {
            self.record(Transition {
                at: self.at,
                code: Disposition::NoTrigger,
                from: Some(finding.severity),
                to: Some(Severity::Minor),
            });
            finding.severity = Severity::Minor;
        }
    }

    /// Step 6: base `suppress` rules, then owner dismissals, then gate duplicates. `Ok` holds
    /// the highest severity of a resembling dismissal the finding exceeds.
    fn suppress(
        &self,
        finding: &Finding,
        anchored: &Anchored,
    ) -> Result<Option<Severity>, Disposition> {
        if self
            .inputs
            .suppress
            .suppresses(finding, &anchored.file_at_head)
        {
            return Err(Disposition::SuppressedByRule);
        }
        let anchor = finding.anchor(&anchored.file_at_head, &anchored.enclosing_symbol);
        let mut previously_dismissed = None;
        for dismissal in self.inputs.dismissals {
            match dismissal.judge(&anchor, finding.severity) {
                DismissalMatch::Suppressed => return Err(Disposition::DismissedByOwner),
                DismissalMatch::MoreSevere { dismissed } => {
                    previously_dismissed = previously_dismissed.max(Some(dismissed));
                }
                DismissalMatch::Unrelated => {}
            }
        }
        if self.duplicates_gate(finding, &anchored.file_at_head) {
            return Err(Disposition::DuplicateOfGate);
        }
        Ok(previously_dismissed)
    }

    /// Whether a head-side gate finding of the same class sits in the same file within
    /// [`GATE_DUPLICATE_LINES`] lines. Base-side lines number another text, so never match.
    fn duplicates_gate(&self, finding: &Finding, file_at_head: &str) -> bool {
        finding.side == Some(Side::Right)
            && self.inputs.gate_findings.iter().any(|gate| {
                gate.class == finding.class
                    && gate.side != Some(Side::Left)
                    && gate.file == file_at_head
                    && gate.line.get().abs_diff(finding.line.get()) <= GATE_DUPLICATE_LINES
            })
    }
}

/// Step 1 for one finding: the schema's conditional requirements, which the domain type cannot
/// state, hold.
fn shape(finding: &Finding) -> Result<(), Disposition> {
    let omission_without_expectation =
        finding.kind == FindingKind::Omission && finding.expected_behavior.is_none();
    let interaction_without_caller =
        finding.introduced_by == Some(IntroducedBy::Interaction) && finding.caller.is_none();
    if omission_without_expectation || interaction_without_caller {
        Err(Disposition::Schema)
    } else {
        Ok(())
    }
}

/// Step 2: finds the evidence in the cited file (base for `LEFT` and deleted code), relocates
/// `line` to the nearest match, checks an omission anchors on a signature and an interaction's
/// caller evidence exists, and computes the fingerprint.
fn anchor(change: &Change, finding: &mut Finding) -> Result<Anchored, Disposition> {
    let located = change.locate(&finding.file, finding.side.unwrap_or(Side::Right));
    let text = located.text.ok_or(Disposition::EvidenceMissing)?;
    let matches = evidence::find(text, &finding.evidence);
    let line =
        evidence::nearest(&matches, finding.line.get()).ok_or(Disposition::EvidenceMissing)?;
    let on_signature = line_text(text, line)
        .and_then(symbol::signature_name)
        .is_some();
    if finding.kind == FindingKind::Omission && !on_signature {
        return Err(Disposition::EvidenceMissing);
    }
    if finding.introduced_by == Some(IntroducedBy::Interaction) {
        relocate_caller(change, finding)?;
    }
    let functions = symbol::enclosing_functions(text);
    // A line past the end of `text` (none is, every match is a line of it) has no function.
    let function_at = |line: u64| {
        usize::try_from(line.saturating_sub(1))
            .ok()
            .and_then(|index| functions.get(index))
            .copied()
    };
    let enclosing_symbol = function_at(line).unwrap_or_default();
    let earlier = matches
        .iter()
        .filter(|&&other| other < line && function_at(other) == Some(enclosing_symbol))
        .count();
    // One match per line at most, and no file read here has 4 billion lines.
    let occurrence_index = u32::try_from(earlier).unwrap_or(u32::MAX);
    let fingerprint = finding
        .anchor(&located.file_at_head, enclosing_symbol)
        .fingerprint(occurrence_index)
        .ok_or(Disposition::EvidenceMissing)?;
    finding.end_line = shifted(finding.end_line, finding.line.get(), line);
    finding.line = NonZeroU64::new(line).ok_or(Disposition::EvidenceMissing)?;
    finding.side = Some(located.side);
    Ok(Anchored {
        in_diff: located.in_diff(line),
        file_at_head: located.file_at_head,
        enclosing_symbol: enclosing_symbol.to_owned(),
        occurrence_index,
        fingerprint,
    })
}

/// Relocates an interaction's `caller.line` to its evidence at head, which must exist.
fn relocate_caller(change: &Change, finding: &mut Finding) -> Result<(), Disposition> {
    let caller = finding.caller.as_mut().ok_or(Disposition::Schema)?;
    let text = change
        .locate(&caller.file, Side::Right)
        .text
        .ok_or(Disposition::EvidenceMissing)?;
    let matches = evidence::find(text, &caller.evidence);
    let line =
        evidence::nearest(&matches, caller.line.get()).ok_or(Disposition::EvidenceMissing)?;
    caller.line = NonZeroU64::new(line).ok_or(Disposition::EvidenceMissing)?;
    Ok(())
}

/// `end_line` moved with `line` from `from` to `to`, keeping the span; dropped if it preceded
/// `from`.
fn shifted(end_line: Option<NonZeroU64>, from: u64, to: u64) -> Option<NonZeroU64> {
    let span = end_line?.get().checked_sub(from)?;
    NonZeroU64::new(to.checked_add(span)?)
}

/// The text of `line` (1-based), if `text` has it.
fn line_text(text: &str, line: u64) -> Option<&str> {
    let index = usize::try_from(line.checked_sub(1)?).ok()?;
    text.lines().nth(index)
}

/// The free text a reviewer wrote, as the injection screen reads it. Evidence and the caller's
/// evidence are quotes step 2 found verbatim in the tree, so they are not screened.
fn free_text(finding: &Finding) -> impl Iterator<Item = &str> {
    [
        Some(&finding.title),
        Some(&finding.body),
        finding.fix.as_ref(),
        finding.trigger.as_ref(),
        finding.expected_behavior.as_ref(),
        finding.group_hint.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(String::as_str)
}
