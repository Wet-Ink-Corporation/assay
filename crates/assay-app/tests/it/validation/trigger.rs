//! Step 4: a role's severity ceiling applies, and from `major` up a trigger is required, else
//! the finding is capped at `minor`.

use std::error::Error;
use std::time::Duration;

use assay_app::{FindingRef, Role, RoleOutcome, Transition, run_specialists};
use assay_domain::{Aeu, Disposition, RequiredRole, Severity};
use serde_json::json;

use super::fixture::{Run, document_by, finding_with, validate_one};
use crate::specialists_fakes::{Answer, Fake, Hold, bundle, document_with, ran, request};

#[test]
fn caps_a_trigger_without_alphanumeric_content() -> Result<(), Box<dyn Error>> {
    let triggers = [".", "\u{200b}\u{2060}", " - ", "\u{feff}!"];
    let findings: Vec<_> = triggers
        .iter()
        .map(|trigger| finding_with(json!({ "trigger": trigger })))
        .collect();

    let validated = validate_one(&findings)?;

    let severities: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.severity)
        .collect();
    assert_eq!(severities, [Severity::Minor; 4]);
    Ok(())
}

#[test]
fn caps_a_combined_micro_blocker_at_its_role_s_ceiling() -> Result<(), Box<dyn Error>> {
    let blocker = finding_with(json!({ "severity": "blocker" }));

    let validated = Run::default().validate(&[document_by("combined-micro", &[blocker])])?;

    let moves: Vec<_> = validated
        .transitions
        .iter()
        .map(|t| (t.code, t.from, t.to))
        .collect();
    assert_eq!(
        moves,
        [(
            Disposition::CapExceeded,
            Some(Severity::Blocker),
            Some(Severity::Major)
        )]
    );
    Ok(())
}

// #173: the role ceiling holds only because the fan-out, not the model, writes `source.role`.
#[test]
fn a_micro_reviewer_claiming_combined_is_still_capped_at_major() -> Result<(), Box<dyn Error>> {
    let blocker = finding_with(json!({ "severity": "blocker" }));
    let claimed = document_with("combined", &[blocker]);
    let fake = Fake::new(
        [(
            Role::CombinedMicro,
            vec![Answer::Ran(ran(claimed, Aeu::ZERO))],
        )],
        Hold::Never,
    );
    let bundle = bundle()?;
    let roles = [RequiredRole::CombinedMicro];
    let specialists = run_specialists(&fake, request(&roles, &bundle, Duration::ZERO))?;
    let document = match specialists.roles.first().map(|report| &report.outcome) {
        Some(RoleOutcome::Completed(structured)) => structured.as_value().to_string(),
        other => return Err(format!("micro did not complete: {other:?}").into()),
    };

    let validated = Run::default().validate(&[document.into_bytes()])?;

    let moves: Vec<_> = validated
        .transitions
        .iter()
        .map(|t| (t.code, t.from, t.to))
        .collect();
    assert_eq!(
        moves,
        [(
            Disposition::CapExceeded,
            Some(Severity::Blocker),
            Some(Severity::Major)
        )]
    );
    Ok(())
}

#[test]
fn leaves_a_combined_blocker_at_blocker() -> Result<(), Box<dyn Error>> {
    let blocker = finding_with(json!({ "severity": "blocker" }));

    let validated = Run::default().validate(&[document_by("combined", &[blocker])])?;

    let severities: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.severity)
        .collect();
    assert_eq!(
        (severities, validated.transitions),
        (vec![Severity::Blocker], vec![])
    );
    Ok(())
}

#[test]
fn caps_a_major_without_a_trigger_at_minor() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(json!({ "trigger": null }))])?;

    let severities: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.severity)
        .collect();
    assert_eq!(
        (severities, validated.transitions),
        (
            vec![Severity::Minor],
            vec![Transition {
                at: FindingRef::Finding {
                    document: 0,
                    index: 0
                },
                code: Disposition::NoTrigger,
                from: Some(Severity::Major),
                to: Some(Severity::Minor),
            }]
        )
    );
    Ok(())
}

#[test]
fn caps_a_blocker_whose_trigger_is_blank() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(
        json!({ "severity": "blocker", "trigger": "  \n" }),
    )])?;

    let moves: Vec<_> = validated
        .transitions
        .iter()
        .map(|t| (t.code, t.from, t.to))
        .collect();
    assert_eq!(
        moves,
        [(
            Disposition::NoTrigger,
            Some(Severity::Blocker),
            Some(Severity::Minor)
        )]
    );
    Ok(())
}

#[test]
fn leaves_a_minor_without_a_trigger_alone() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(
        json!({ "severity": "minor", "trigger": null }),
    )])?;

    let severities: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.severity)
        .collect();
    assert_eq!(
        (severities, validated.transitions),
        (vec![Severity::Minor], vec![])
    );
    Ok(())
}
