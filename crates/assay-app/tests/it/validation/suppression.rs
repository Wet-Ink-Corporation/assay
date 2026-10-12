//! Step 6: base `suppress` rules and owner dismissals, then (Z4) duplicates of gate findings.

use std::error::Error;

use assay_app::SuppressRules;
use assay_domain::{Anchor, Class, Dismissal, Disposition, Finding, Severity};
use serde_json::{Value, json};

use super::fixture::{LIB, Run, document, finding, finding_with};

/// Suppresses every finding of one class.
#[derive(Debug)]
struct SuppressClass(Class);

impl SuppressRules for SuppressClass {
    fn suppresses(&self, finding: &Finding, _file_at_head: &str) -> bool {
        finding.class == self.0
    }
}

fn codes(run: &Run<'_>, findings: &[Value]) -> Result<Vec<Disposition>, Box<dyn Error>> {
    let validated = run.validate(&[document(findings)])?;
    Ok(validated.transitions.iter().map(|t| t.code).collect())
}

#[test]
fn drops_a_finding_a_base_rule_suppresses() -> Result<(), Box<dyn Error>> {
    let rules = SuppressClass(Class::ErrorHandling);
    let run = Run {
        suppress: &rules,
        ..Run::default()
    };

    let codes = codes(
        &run,
        &[finding(), finding_with(json!({ "class": "logic" }))],
    )?;

    assert_eq!(codes, [Disposition::SuppressedByRule]);
    Ok(())
}

/// An owner's dismissal of the default finding, as a `major`.
fn dismissed_major() -> Dismissal<'static> {
    Dismissal {
        anchor: Anchor {
            class: Class::ErrorHandling,
            file: LIB,
            enclosing_symbol: "parse",
            evidence: "let x = input.parse().unwrap(); // reworded",
            expected_behavior: None,
        },
        severity: Severity::Major,
    }
}

#[test]
fn drops_a_finding_an_owner_dismissed_at_or_above_its_severity() -> Result<(), Box<dyn Error>> {
    let run = Run {
        dismissals: vec![dismissed_major()],
        ..Run::default()
    };

    let codes = codes(&run, &[finding()])?;

    assert_eq!(codes, [Disposition::DismissedByOwner]);
    Ok(())
}

#[test]
fn keeps_a_more_severe_resembling_finding_noting_the_dismissal() -> Result<(), Box<dyn Error>> {
    let run = Run {
        dismissals: vec![dismissed_major()],
        ..Run::default()
    };

    let validated = run.validate(&[document(&[finding_with(json!({ "severity": "blocker" }))])])?;

    let notes: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.previously_dismissed)
        .collect();
    assert_eq!(notes, [Some(Severity::Major)]);
    Ok(())
}

/// A gate finding of `class` on line `line` of `src/lib.rs`.
fn gate_finding(class: &str, line: u64) -> Result<Finding, serde_json::Error> {
    serde_json::from_value(finding_with(json!({ "class": class, "line": line })))
}

#[test]
fn drops_a_duplicate_of_a_gate_finding_within_three_lines() -> Result<(), Box<dyn Error>> {
    let run = Run {
        gate_findings: vec![gate_finding("error-handling", 5)?],
        ..Run::default()
    };

    let codes = codes(&run, &[finding()])?;

    assert_eq!(codes, [Disposition::DuplicateOfGate]);
    Ok(())
}

#[test]
fn keeps_findings_four_lines_from_or_unlike_a_gate_finding() -> Result<(), Box<dyn Error>> {
    let run = Run {
        gate_findings: vec![
            gate_finding("error-handling", 6)?,
            gate_finding("logic", 2)?,
        ],
        ..Run::default()
    };

    let codes = codes(&run, &[finding()])?;

    assert_eq!(codes, []);
    Ok(())
}
