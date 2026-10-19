//! Properties over generated findings: determinism, relocation onto matching lines only, that
//! every finding ends kept or dropped exactly once, and that every event validation emits
//! passes the read rule (#264).

use assay_app::{FindingRef, Role, SuppressRules, Validated};
use assay_domain::{Disposition, Finding, Side, normalise};
use proptest::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use super::fixture::{LIB_AT_BASE, LIB_AT_HEAD, Run, RunError, document, finding_with};
use crate::support::read_back;

/// A base `suppress` rule that matches every finding.
struct SuppressAll;

impl SuppressRules for SuppressAll {
    fn suppresses(&self, _finding: &Finding, _file_at_head: &str) -> bool {
        true
    }
}

/// Dispositions that drop a finding; the others keep it.
const DROPPING: [Disposition; 7] = [
    Disposition::Schema,
    Disposition::EvidenceMissing,
    Disposition::InjectionSuspect,
    Disposition::SuppressedByRule,
    Disposition::DismissedByOwner,
    Disposition::DuplicateOfGate,
    Disposition::Duplicate,
];

/// Spans the generated findings quote: on one or more lines of either side, spanning lines,
/// too short to locate, and nowhere.
const EVIDENCE: [&str; 7] = [
    "let x = input.parse().unwrap();",
    "input.parse().unwrap(); ... x",
    "Ok(x)",
    "Ok(input.len())",
    "x",
    "pub fn other(input: &str) -> u32 {",
    "absent_function();",
];

fn generated_finding() -> impl Strategy<Value = Value> {
    (
        1_u64..15,
        prop::sample::select(EVIDENCE.to_vec()),
        prop::sample::select(vec!["blocker", "major", "minor", "nit"]),
        any::<bool>(),
        any::<bool>(),
        prop::sample::select(vec!["RIGHT", "LEFT"]),
    )
        .prop_map(
            |(line, evidence, severity, triggered, pre_existing, side)| {
                let trigger = if triggered {
                    json!("input \"a\" reaches it")
                } else {
                    Value::Null
                };
                finding_with(json!({
                    "line": line,
                    "evidence": evidence,
                    "severity": severity,
                    "trigger": trigger,
                    "pre_existing": pre_existing,
                    "side": side,
                }))
            },
        )
}

fn validated(findings: &[Value]) -> Result<Validated, RunError> {
    Run::default().validate(&[document(findings)])
}

proptest! {
    #[test]
    fn validation_is_deterministic(findings in prop::collection::vec(generated_finding(), 0..8)) {
        prop_assert_eq!(validated(&findings)?, validated(&findings)?);
    }

    #[test]
    fn a_kept_finding_sits_on_a_line_holding_its_evidence(
        findings in prop::collection::vec(generated_finding(), 0..8),
    ) {
        for kept in validated(&findings)?.findings {
            let side = kept.finding.side.expect("validation makes the side explicit");
            let text = match side {
                Side::Right => LIB_AT_HEAD,
                Side::Left => LIB_AT_BASE,
            };
            let FindingRef::Finding { index, .. } = kept.at else {
                return Err(TestCaseError::fail("a kept finding refers to a finding"));
            };
            let quoted = findings[index]["evidence"].as_str().expect("generated evidence");
            let first_piece = normalise(quoted)
                .split('\u{2026}')
                .find(|piece| !piece.is_empty())
                .expect("generated evidence has content")
                .to_owned();
            let line_index = usize::try_from(kept.finding.line.get() - 1).expect("lines fit");
            let line = text.lines().nth(line_index).expect("the kept line is in the tree");
            // Every generated quote's first piece fits on one line, so the tree's line at the
            // kept `line` holds it; and the evidence now starts with that tree line.
            prop_assert!(
                normalise(line).contains(&first_piece),
                "line {} on {:?} does not hold {:?}", kept.finding.line, side, quoted
            );
            prop_assert_eq!(Some(line), kept.finding.evidence.lines().next());
        }
    }

    #[test]
    fn every_finding_is_kept_or_dropped_exactly_once(
        findings in prop::collection::vec(generated_finding(), 0..8),
    ) {
        let validated = validated(&findings)?;

        for index in 0..findings.len() {
            let at = FindingRef::Finding { document: 0, index };
            let kept = validated.findings.iter().filter(|kept| kept.at == at).count();
            let dropped = validated
                .transitions
                .iter()
                .filter(|t| t.at == at && DROPPING.contains(&t.code))
                .count();
            prop_assert_eq!(
                kept + dropped,
                1,
                "finding {} is kept {} and dropped {} times",
                index,
                kept,
                dropped
            );
        }
    }

    /// Whatever the role's ceiling, a gate reporting the first finding, and a suppress rule or
    /// not, every `FindingDisposed` validation emits reads back as an event: no code that
    /// requires a fingerprint is written without one (#264).
    #[test]
    fn every_event_validation_emits_passes_the_read_rule(
        findings in prop::collection::vec(generated_finding(), 0..8),
        role in prop::sample::select(Role::ALL.to_vec()),
        gate_reports_first in any::<bool>(),
        suppressed in any::<bool>(),
    ) {
        let mut run = Run::default();
        if gate_reports_first && let Some(first) = findings.first() {
            run.gate_findings.push(Finding::deserialize(first)?);
        }
        if suppressed {
            run.suppress = &SuppressAll;
        }

        let validated = run.validate_as(role, &[document(&findings)])?;

        for payload in validated.events() {
            let read = read_back(payload);
            prop_assert!(read.is_ok(), "an emitted event is refused on read: {:?}", read);
        }
    }
}
