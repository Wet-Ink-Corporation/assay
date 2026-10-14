//! Step 4: a role's severity ceiling applies, and from `major` up a trigger is required, else
//! the finding is capped at `minor`.

use std::error::Error;
use std::time::Duration;

use assay_app::{FindingRef, Role, RoleOutcome, Transition, Validated, run_specialists};
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

    let validated = Run::default().validate_as(
        Role::CombinedMicro,
        &[document_by("combined-micro", &[blocker])],
    )?;

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

/// A blocker from `role`'s call, whose output claims `claimed`, run through the fan-out and
/// validated as the role its report names: the path every tier's reviewer output takes.
fn blocker_through_fan_out(role: RequiredRole, claimed: &str) -> Result<Validated, Box<dyn Error>> {
    let blocker = finding_with(json!({ "severity": "blocker" }));
    let output = document_with(claimed, &[blocker]);
    let fake = Fake::new(
        [(Role::from(role), vec![Answer::Ran(ran(output, Aeu::ZERO))])],
        Hold::Never,
    );
    let bundle = bundle()?;
    let roles = [role];
    let specialists = run_specialists(&fake, request(&roles, &bundle, Duration::ZERO))?;
    let report = specialists.roles.first().ok_or("the role was due")?;
    let RoleOutcome::Completed(structured) = &report.outcome else {
        return Err(format!("{role:?} did not complete: {:?}", report.outcome).into());
    };
    let document = structured.as_value().to_string().into_bytes();

    Ok(Run::default().validate_as(report.role, &[document])?)
}

/// The severities `validated` kept.
fn kept_severities(validated: &Validated) -> Vec<Severity> {
    validated
        .findings
        .iter()
        .map(|kept| kept.finding.severity)
        .collect()
}

// #173: the fan-out stamps `source.role` over the model's claim, and validation takes the role
// from the report, so the micro ceiling holds end to end.
#[test]
fn a_micro_reviewer_claiming_combined_is_still_capped_at_major() -> Result<(), Box<dyn Error>> {
    let validated = blocker_through_fan_out(RequiredRole::CombinedMicro, "combined")?;

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

// #173, the lite path: a `combined` call claiming `combined-micro` is neither refused nor capped.
#[test]
fn a_lite_reviewer_claiming_micro_keeps_its_own_ceiling() -> Result<(), Box<dyn Error>> {
    let validated = blocker_through_fan_out(RequiredRole::Combined, "combined-micro")?;

    assert_eq!(
        (kept_severities(&validated), validated.transitions),
        (vec![Severity::Blocker], vec![])
    );
    Ok(())
}

// #173, the full path: each specialist is validated as itself, whatever its output claims.
#[test]
fn full_tier_specialists_claiming_micro_keep_their_own_ceiling() -> Result<(), Box<dyn Error>> {
    let specialists = [
        RequiredRole::Bugs,
        RequiredRole::Security,
        RequiredRole::Design,
        RequiredRole::Meta,
    ];

    let kept = specialists
        .into_iter()
        .map(|role| {
            Ok((
                role,
                kept_severities(&blocker_through_fan_out(role, "combined-micro")?),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    assert_eq!(
        kept,
        specialists.map(|role| (role, vec![Severity::Blocker]))
    );
    Ok(())
}

// #173: the ceiling is the caller's role's, so the document's own claim cannot lift it; a claim
// that disagrees means the output reached validation unstamped, and it is refused whole.
#[test]
fn refuses_a_micro_document_that_claims_combined_as_schema() -> Result<(), Box<dyn Error>> {
    let blocker = finding_with(json!({ "severity": "blocker" }));

    let validated =
        Run::default().validate_as(Role::CombinedMicro, &[document_by("combined", &[blocker])])?;

    assert_eq!(
        (validated.transitions, validated.findings.len()),
        (vec![REFUSED_WHOLE], 0)
    );
    Ok(())
}

/// The one transition a document refused whole at step 1 records.
const REFUSED_WHOLE: Transition = Transition {
    at: FindingRef::Document { document: 0 },
    code: Disposition::Schema,
    from: None,
    to: None,
};

// #173, the downgrade direction: a claim below the caller's role is refused too, not believed,
// so an unstamped document never has its ceiling chosen by the model either way.
#[test]
fn refuses_a_bugs_document_that_claims_combined_micro_as_schema() -> Result<(), Box<dyn Error>> {
    let blocker = finding_with(json!({ "severity": "blocker" }));

    let validated =
        Run::default().validate_as(Role::Bugs, &[document_by("combined-micro", &[blocker])])?;

    assert_eq!(
        (validated.transitions, validated.findings.len()),
        (vec![REFUSED_WHOLE], 0)
    );
    Ok(())
}

// #173: a document with no `source.role` cannot be matched to the caller's role, so it is
// refused whole rather than assumed to be that role's.
#[test]
fn refuses_a_document_without_a_role_as_schema() -> Result<(), Box<dyn Error>> {
    let blocker = finding_with(json!({ "severity": "blocker" }));
    let mut document: serde_json::Value =
        serde_json::from_slice(&document_by("combined", &[blocker]))?;
    let source = document
        .get_mut("source")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("the fixture has a source object")?;
    source.remove("role").ok_or("the fixture names a role")?;

    let validated =
        Run::default().validate_as(Role::Combined, &[serde_json::to_vec(&document)?])?;

    assert_eq!(
        (validated.transitions, validated.findings.len()),
        (vec![REFUSED_WHOLE], 0)
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
