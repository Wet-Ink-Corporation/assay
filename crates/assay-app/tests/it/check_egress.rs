//! `check_local`'s egress scan: model text is scanned as Z4 scans it, with no token and the
//! snapshot's files and the run's commits as what explains a long run (03 §3, #87, wi-b5affa).

use std::error::Error;

use std::assert_matches;

use assay_app::{CheckError, CheckPorts, LocalReport, ReviewStatus, check_local};
use serde_json::Value;

use crate::check::{check, validator};
use crate::check_fakes::{
    FakeGates, FakeModel, FakePatches, Setup, Unreadable, dirty_tree, unwrap_finding,
};
use crate::plan_fakes::{FakeError, HEAD};

/// The `unwrap` finding with `body` replaced.
fn finding_saying(body: &str) -> Value {
    let mut finding = unwrap_finding();
    if let Some(fields) = finding.as_object_mut() {
        fields.insert("body".to_owned(), Value::from(body));
    }
    finding
}

#[test]
fn a_long_run_the_snapshot_does_not_hold_withholds_the_review() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::finding(&[finding_saying("leaked Zq8xWv3TpL0aRk7NcY2mJh5U here")]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    let review = report.review();
    assert_eq!(review.status, ReviewStatus::Unavailable);
    assert!(review.findings.is_empty(), "no model text is shown");
    let note = review.note.as_deref().ok_or("an alarm is explained")?;
    assert!(note.contains("egress"), "{note}");
    Ok(())
}

#[test]
fn a_long_run_from_an_unchanged_file_of_the_snapshot_is_explained() -> Result<(), Box<dyn Error>> {
    let mut pr = dirty_tree();
    pr.write(
        HEAD,
        "src/names.rs",
        b"fn parseInputFallbackValue() -> u32 { 0 }\n",
    );
    let model = FakeModel::finding(&[finding_saying("see parseInputFallbackValue")]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.review().status, ReviewStatus::Completed);
    assert_eq!(report.review().findings.len(), 1);
    Ok(())
}

#[test]
fn the_snapshot_commit_is_explained() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::finding(&[finding_saying(&format!("seen at {HEAD}"))]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.review().status, ReviewStatus::Completed);
    assert_eq!(report.review().findings.len(), 1);
    Ok(())
}

/// What `check_local` returns over the fakes.
type Checked = Result<LocalReport, CheckError<FakeError, FakeError>>;

/// Checks [`dirty_tree`] plus an unchanged `src/names.rs` that cannot be read, or comes back
/// one blob short.
fn check_with_unreadable_names(short: bool) -> Result<Checked, Box<dyn Error>> {
    let mut pr = dirty_tree();
    pr.write(
        HEAD,
        "src/names.rs",
        b"fn parseInputFallbackValue() -> u32 { 0 }\n",
    );
    let setup = Setup::of(&pr)?;
    let objects = Unreadable {
        pr: &pr,
        path: "src/names.rs",
        short,
    };
    let ports = CheckPorts {
        objects: &objects,
        patches: &FakePatches::lib(),
        gates: &mut FakeGates::passing(),
        model: &FakeModel::finding(&[]),
    };
    Ok(check_local(&setup.request(), ports, &validator()?))
}

#[test]
fn an_unreadable_file_of_the_snapshot_fails_the_check() -> Result<(), Box<dyn Error>> {
    let result = check_with_unreadable_names(false)?;

    assert_matches!(result, Err(CheckError::Objects(_)));
    Ok(())
}

#[test]
fn a_short_read_of_the_snapshot_fails_the_check() -> Result<(), Box<dyn Error>> {
    let result = check_with_unreadable_names(true)?;

    assert_matches!(result, Err(CheckError::ShortRead { .. }));
    Ok(())
}
