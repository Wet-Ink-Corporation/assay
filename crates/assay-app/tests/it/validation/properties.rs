//! Properties over generated findings: determinism, relocation onto matching lines only, and
//! that every finding ends kept or dropped exactly once.

use assay_app::{ContractSchemaError, FindingRef, Validated};
use assay_domain::{Disposition, Side};
use proptest::prelude::*;
use serde_json::{Value, json};

use super::fixture::{LIB_AT_BASE, LIB_AT_HEAD, Run, document, finding_with};

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

/// Spans the generated findings quote: some on one or more lines of either side, one nowhere.
const EVIDENCE: [&str; 6] = [
    "let x = input.parse().unwrap();",
    "Ok(x)",
    "Ok(input.len())",
    "x",
    "pub fn other(input: &str) -> u32 {",
    "absent();",
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

fn validated(findings: &[Value]) -> Result<Validated, &'static ContractSchemaError> {
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
            let index = usize::try_from(kept.finding.line.get() - 1).expect("lines fit usize");
            let line = text.lines().nth(index);
            prop_assert!(
                line.is_some_and(|line| line.contains(&kept.finding.evidence)),
                "line {} on {:?} does not hold {:?}", kept.finding.line, side, kept.finding.evidence
            );
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
            prop_assert_eq!(kept + dropped, 1, "finding {} is kept {} and dropped {} times", index, kept, dropped);
        }
    }
}
