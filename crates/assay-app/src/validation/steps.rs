//! Steps 2 to 6 of 04 §4 for each finding of a run, in order; a dropping step ends the finding.

use assay_domain::{
    DismissalMatch, Disposition, Finding, FindingKind, Fingerprint, IntroducedBy, Severity, Side,
};

use super::anchor::{Anchored, anchor};
use super::index::Files;
use super::{
    EgressHit, FindingRef, FreeTextField, Inputs, Placement, Transition, ValidFinding, Validated,
    screen,
};

/// How many lines apart a reviewer finding may be from a gate finding of the same class in the
/// same file and still duplicate it (04 §1).
const GATE_DUPLICATE_LINES: u64 = 3;

/// The most bytes a `rule` may hold (Weigh-In default wi-641e1a): the schema leaves it
/// unbounded, and it is model text that reaches the output.
const MAX_RULE_BYTES: usize = 128;

/// The most characters a `group_hint` may hold, in line with the schema's caps on the other
/// short free-text fields (`title`); the schema leaves it unbounded (wi-641e1a).
const MAX_GROUP_HINT_CHARS: usize = 100;

/// One validation run: its inputs, the files indexed so far and the outcome so far.
pub(super) struct Run<'s, 'c> {
    inputs: &'s Inputs<'c>,
    files: Files<'c>,
    validated: Validated,
}

impl<'s, 'c> Run<'s, 'c> {
    /// A run over `inputs` that has validated nothing yet.
    pub(super) fn new(inputs: &'s Inputs<'c>) -> Self {
        Self {
            inputs,
            files: Files::default(),
            validated: Validated {
                findings: Vec::new(),
                transitions: Vec::new(),
                egress: Vec::new(),
            },
        }
    }

    /// Records that the input at `at` was dropped as `code` before its evidence was located, so
    /// with no fingerprint.
    pub(super) fn reject(&mut self, at: FindingRef, code: Disposition) {
        self.validated
            .transitions
            .push(Transition::of(at, code, None));
    }

    /// Records that the document at `at`, whose bytes are `document`, was dropped as `code`
    /// before its findings could be read; as `injection-suspect` instead, with an
    /// [`EgressHit`], when it holds the token (Z3 only).
    pub(super) fn reject_document(&mut self, at: FindingRef, document: &[u8], code: Disposition) {
        match self.inputs.egress.scan_rejected(document) {
            Some(kind) => {
                self.validated.egress.push(EgressHit {
                    at,
                    field: FreeTextField::RejectedDocument,
                    kind,
                });
                self.reject(at, Disposition::InjectionSuspect);
            }
            None => self.reject(at, code),
        }
    }

    /// The egress scan (03 §3) of one schema-valid finding, before any other step reads it:
    /// `true` when it hit, recorded as an [`EgressHit`] and an `injection-suspect` drop.
    pub(super) fn drops_for_egress(&mut self, at: FindingRef, finding: &Finding) -> bool {
        let Some((field, kind)) = self.inputs.egress.scan_finding(finding) else {
            return false;
        };
        self.validated.egress.push(EgressHit { at, field, kind });
        self.reject(at, Disposition::InjectionSuspect);
        true
    }

    /// Validates one schema-valid finding from a role whose findings may be at most `ceiling`.
    pub(super) fn check(&mut self, at: FindingRef, finding: Finding, ceiling: Severity) {
        let mut steps = Steps {
            inputs: self.inputs,
            files: &mut self.files,
            at,
            ceiling,
            fp: None,
            transitions: &mut self.validated.transitions,
        };
        match steps.apply(finding) {
            Ok(kept) => self.validated.findings.push(kept),
            Err(code) => steps.record(code),
        }
    }

    /// The outcome of the run.
    pub(super) fn finish(self) -> Validated {
        self.validated
    }
}

/// One finding on its way through the steps.
struct Steps<'r, 'c> {
    inputs: &'r Inputs<'c>,
    files: &'r mut Files<'c>,
    at: FindingRef,
    ceiling: Severity,
    /// Its fingerprint, from step 2 on; every transition recorded after that names it.
    fp: Option<Fingerprint>,
    transitions: &'r mut Vec<Transition>,
}

impl Steps<'_, '_> {
    /// Steps 2 to 6; `Err` is the disposition that drops the finding.
    fn apply(&mut self, mut finding: Finding) -> Result<ValidFinding, Disposition> {
        shape(&finding)?;
        let anchored = anchor(self.inputs.change, self.files, &mut finding)?;
        self.fp = Some(anchored.fingerprint);
        let placement = self.place(&mut finding, &anchored);
        self.cap(&mut finding);
        if free_text(&finding, anchored.quoted_caller.as_deref())
            .any(|text| screen::screen(text, self.inputs.repository).is_some())
        {
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

    fn record(&mut self, code: Disposition) {
        self.transitions
            .push(Transition::of(self.at, code, self.fp));
    }

    /// A transition that moves `finding` to `to`, recorded as `code`.
    fn lower(&mut self, finding: &mut Finding, code: Disposition, to: Severity) {
        self.transitions.push(Transition {
            at: self.at,
            code,
            fp: self.fp,
            from: Some(finding.severity),
            to: Some(to),
        });
        finding.severity = to;
    }

    /// Step 3: inline only when its lines are in one hunk, else moved to the summary;
    /// `pre_existing` honoured only outside the diff and not for an interaction.
    fn place(&mut self, finding: &mut Finding, anchored: &Anchored) -> Placement {
        let placement = if anchored.inline {
            Placement::Inline
        } else {
            self.record(Disposition::LineNotInDiffMoved);
            Placement::Summary
        };
        if finding.pre_existing {
            let interaction = finding.introduced_by == Some(IntroducedBy::Interaction);
            if anchored.in_diff || interaction {
                finding.pre_existing = false;
            } else {
                self.record(Disposition::PreExisting);
            }
        }
        placement
    }

    /// Step 4: the role's severity ceiling (04 §2.3), recorded as `cap-exceeded`; then, from
    /// `major` up, a trigger with alphanumeric content is required, else the finding is capped
    /// at `minor`.
    fn cap(&mut self, finding: &mut Finding) {
        if finding.severity > self.ceiling {
            self.lower(finding, Disposition::CapExceeded, self.ceiling);
        }
        let triggered = finding.trigger.as_deref().is_some_and(has_content);
        if finding.severity >= Severity::Major && !triggered {
            self.lower(finding, Disposition::NoTrigger, Severity::Minor);
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

/// Whether `trigger` names something: an alphanumeric character other than an invisible one
/// (zero-width and format characters, which the screen folds away too).
fn has_content(trigger: &str) -> bool {
    trigger
        .chars()
        .any(|c| c.is_alphanumeric() && !screen::is_invisible(c))
}

/// Step 1 for one finding: the schema's conditional requirements, which the domain type cannot
/// state, hold; only an interaction has a caller; and the free-text fields the schema leaves
/// unbounded are within their caps.
fn shape(finding: &Finding) -> Result<(), Disposition> {
    let omission_without_expectation =
        finding.kind == FindingKind::Omission && finding.expected_behavior.is_none();
    // A caller is model text only an interaction's step 2 checks and replaces; on any other
    // finding it would reach the output as written, so it must be absent.
    let interaction = finding.introduced_by == Some(IntroducedBy::Interaction);
    let caller_mismatch = interaction != finding.caller.is_some();
    let over_cap = finding
        .rule
        .as_ref()
        .is_some_and(|rule| rule.len() > MAX_RULE_BYTES)
        || finding
            .group_hint
            .as_ref()
            .is_some_and(|hint| hint.chars().count() > MAX_GROUP_HINT_CHARS);
    if omission_without_expectation || caller_mismatch || over_cap {
        Err(Disposition::Schema)
    } else {
        Ok(())
    }
}

/// The text the injection screen reads: everything the reviewer wrote, `rule` included, and an
/// interaction's caller evidence as the reviewer quoted it (`quoted_caller`). The evidence and
/// caller evidence the finding now holds are the tree's lines (step 2 replaced them): repository
/// content, published as code, so not screened (wi-d4735c).
fn free_text<'f>(
    finding: &'f Finding,
    quoted_caller: Option<&'f str>,
) -> impl Iterator<Item = &'f str> {
    [
        Some(finding.title.as_str()),
        Some(finding.body.as_str()),
        finding.fix.as_deref(),
        finding.trigger.as_deref(),
        finding.expected_behavior.as_deref(),
        finding.group_hint.as_deref(),
        finding.rule.as_deref(),
        quoted_caller,
    ]
    .into_iter()
    .flatten()
}
