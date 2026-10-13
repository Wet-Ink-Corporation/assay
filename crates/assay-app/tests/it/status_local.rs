//! `assay status --local` (#130, 03 §6b): the golden local `assay.status/1` documents of
//! `schemas/fixtures/status/valid/` (fresh, stale and missing local results), which the mod's
//! tests import, each produced here from a report `check_local` wrote; and the refusals of a
//! report that does not read.

use std::error::Error;
use std::fs;

use assay_app::{
    CommitId, ContractValidator, FetchedAt, LocalReportError, LocalStatusError, LocalStatusRequest,
    MAX_CHECK_RUNS, MAX_DOCUMENT_BYTES, MAX_FINDINGS, MAX_REASONS, StatusReport, StoredLocalReport,
    local_status,
};
use serde_json::{Value, json};

use crate::check::check;
use crate::check_fakes::{FakeGates, FakeModel, TREE, dirty_tree, unwrap_finding};
use crate::plan_fakes::HEAD;
use crate::status_fakes::FETCHED_AT;
use crate::support::schemas_dir;

/// The working tree's fingerprint after an edit since the check.
const EDITED: &str = "dddddddddddddddddddddddddddddddddddddddd";

/// A local report with a failing `unit` gate finding and the reviewer's `unwrap` finding.
fn report() -> Result<Vec<u8>, Box<dyn Error>> {
    let gate_finding = json!({
        "class": "docs", "rule": "D-01", "kind": "defect", "severity": "minor",
        "file": "src/lib.rs", "line": 1, "evidence": "pub fn parse(input: &str) -> u32 {",
        "title": "`parse` has no doc comment", "body": "D-01: document every public item.",
    });
    let mut gates = FakeGates::concluding("unit", "fail", &json!([gate_finding]));
    let model = FakeModel::finding(&[unwrap_finding()]);
    Ok(check(&dirty_tree(), &mut gates, &model)?.to_json()?)
}

/// The report as JSON, to edit.
fn report_value() -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(&report()?)?)
}

/// The local status of the working tree at `tree` given `file`, a report named for `named`.
fn status_of(
    tree: &str,
    file: Option<(&str, &[u8])>,
) -> Result<Result<StatusReport, LocalStatusError>, Box<dyn Error>> {
    let head: CommitId = HEAD.parse()?;
    let fetched_at: FetchedAt = FETCHED_AT.parse()?;
    let request = LocalStatusRequest {
        tree,
        local_head: Some(&head),
        report: file.map(|(named, bytes)| StoredLocalReport { tree: named, bytes }),
        fetched_at: &fetched_at,
    };
    Ok(local_status(&request, &ContractValidator::new()?))
}

/// The local status of `value` as the report of [`TREE`], with the working tree at `TREE`.
fn status_of_value(
    value: &Value,
) -> Result<Result<StatusReport, LocalStatusError>, Box<dyn Error>> {
    status_of(TREE, Some((TREE, &serde_json::to_vec(value)?)))
}

/// Asserts the status is exactly the golden `name`.
fn assert_golden(report: &StatusReport, name: &str) -> Result<(), Box<dyn Error>> {
    let path = schemas_dir()
        .join("fixtures/status/valid")
        .join(format!("{name}.json"));
    assert_eq!(
        String::from_utf8(report.json().to_vec())?,
        fs::read_to_string(path)?,
        "golden {name}"
    );
    Ok(())
}

#[test]
fn a_result_for_the_current_tree_is_fresh() -> Result<(), Box<dyn Error>> {
    let bytes = report()?;

    let status = status_of(TREE, Some((TREE, &bytes)))??;

    assert_golden(&status, "local-fresh")
}

#[test]
fn a_result_for_another_tree_is_stale() -> Result<(), Box<dyn Error>> {
    let bytes = report()?;

    let status = status_of(EDITED, Some((TREE, &bytes)))??;

    assert_golden(&status, "local-stale")
}

#[test]
fn no_result_is_a_status_without_a_check() -> Result<(), Box<dyn Error>> {
    let status = status_of(EDITED, None)??;

    assert_golden(&status, "local-missing")
}

#[test]
fn the_text_says_whether_the_result_is_for_this_tree() -> Result<(), Box<dyn Error>> {
    let bytes = report()?;

    let lines = [
        status_of(TREE, Some((TREE, &bytes)))??,
        status_of(EDITED, Some((TREE, &bytes)))??,
        status_of(EDITED, None)??,
    ]
    .map(|status| status.text().lines().nth(2).map(str::to_owned));

    assert_eq!(
        lines,
        [
            Some("Local result: for this tree".to_owned()),
            Some(format!("Local result: stale, for tree {TREE}")),
            Some("Local result: none; run assay check --local".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn the_text_shows_the_check_decision_gates_and_findings() -> Result<(), Box<dyn Error>> {
    let bytes = report()?;

    let status = status_of(TREE, Some((TREE, &bytes)))??;

    let text = status.text();
    assert!(
        text.starts_with("assay status --local: advisory, CI never reads it\n"),
        "{text}"
    );
    for line in [
        "Check: completed failure",
        "Decision: failure (tier micro)",
        "Gates:",
        "  unit failure",
        "Findings:",
        "Fetched: 2026-10-13T09:30:00Z",
    ] {
        assert!(
            text.lines().any(|shown| shown == line),
            "{line:?} in {text}"
        );
    }
    Ok(())
}

#[test]
fn a_fingerprint_is_shown_once_at_its_highest_severity_in_its_first_place()
-> Result<(), Box<dyn Error>> {
    let mut value = report_value()?;
    let review = value["review"]["findings"][0].clone();
    let mut weaker = review.clone();
    weaker["severity"] = json!("nit");
    weaker["title"] = json!("seen first, less severe");
    value["gates"][0]["findings"] = json!([weaker]);
    value["review"]["findings"] = json!([review]);

    let status = status_of_value(&value)??;

    let document: Value = serde_json::from_slice(status.json())?;
    let shown: Vec<(&Value, &Value)> = document["findings"]
        .as_array()
        .ok_or("findings is a list")?
        .iter()
        .map(|finding| (&finding["severity"], &finding["title"]))
        .collect();
    assert_eq!(
        shown,
        [(&json!("major"), &json!("unwrap on user input panics"))]
    );
    Ok(())
}

#[test]
fn a_plugin_in_several_jobs_is_one_gate_concluding_as_its_worst_job() -> Result<(), Box<dyn Error>>
{
    let mut value = report_value()?;
    let mut passed = value["gates"][0].clone();
    passed["outcome"] = json!("pass");
    passed["findings"] = json!([]);
    let mut other = passed.clone();
    other["name"] = json!("lint");
    let mut errored = passed.clone();
    errored["outcome"] = json!("gate-error:not-run");
    value["gates"] = json!([passed, other, errored]);

    let status = status_of_value(&value)??;

    let document: Value = serde_json::from_slice(status.json())?;
    assert_eq!(
        document["gates"],
        json!([
            {"plugin": "unit", "conclusion": "failure"},
            {"plugin": "lint", "conclusion": "success"},
        ])
    );
    Ok(())
}

#[test]
fn a_pending_decision_is_a_check_in_progress() -> Result<(), Box<dyn Error>> {
    let mut value = report_value()?;
    value["decision"]["conclusion"] = json!("pending");

    let status = status_of_value(&value)??;

    let document: Value = serde_json::from_slice(status.json())?;
    assert_eq!(
        document["check"],
        json!({"conclusion": null, "status": "in_progress"})
    );
    Ok(())
}

#[test]
fn a_report_named_for_another_tree_is_refused() -> Result<(), Box<dyn Error>> {
    let bytes = report()?;

    let result = status_of(TREE, Some((EDITED, &bytes)))?;

    assert!(
        matches!(result, Err(LocalStatusError::TreeMismatch)),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_tree_that_is_not_a_tree_id_is_refused() -> Result<(), Box<dyn Error>> {
    let bytes = report()?;
    let upper = TREE.to_uppercase();
    let sha256 = "c".repeat(64);

    let current = ["", "cccc", upper.as_str(), &"c".repeat(41)]
        .map(|tree| status_of(tree, None).map(Result::err));
    let named = status_of(TREE, Some(("../x", &bytes)))?;
    let longer = status_of(&sha256, None)?;

    for refused in current {
        assert!(
            matches!(refused?, Some(LocalStatusError::Tree)),
            "the current tree"
        );
    }
    assert!(matches!(named, Err(LocalStatusError::Tree)), "{named:?}");
    assert!(longer.is_ok(), "a SHA-256 tree id is one: {longer:?}");
    Ok(())
}

/// The error `bytes` gives as the report of [`TREE`].
fn refusal(bytes: &[u8]) -> Result<LocalStatusError, Box<dyn Error>> {
    status_of(TREE, Some((TREE, bytes)))?
        .err()
        .ok_or_else(|| "the report was accepted".into())
}

#[test]
fn a_report_that_is_not_strict_json_of_its_shape_is_refused() -> Result<(), Box<dyn Error>> {
    let text = String::from_utf8(report()?)?;
    let repeated = text.replacen("{\n", "{\n  \"advisory\": true,\n", 1);
    let mut unknown = report_value()?;
    unknown["extra"] = json!(1);
    let mut gate_field = report_value()?;
    gate_field["gates"][0]["url"] = json!("x");
    let mut outcome = report_value()?;
    outcome["gates"][0]["outcome"] = json!("passed");
    let mut fp = report_value()?;
    fp["review"]["findings"][0]["fp"] = json!("ABC");

    let refused = [
        refusal(b"not json")?,
        refusal(repeated.as_bytes())?,
        refusal(&serde_json::to_vec(&unknown)?)?,
        refusal(&serde_json::to_vec(&gate_field)?)?,
        refusal(&serde_json::to_vec(&outcome)?)?,
        refusal(&serde_json::to_vec(&fp)?)?,
    ];

    for error in refused {
        assert!(
            matches!(
                error,
                LocalStatusError::Report(LocalReportError::Malformed(_))
            ),
            "{error:?}"
        );
    }
    Ok(())
}

#[test]
fn a_file_that_is_not_an_advisory_local_report_is_refused() -> Result<(), Box<dyn Error>> {
    let mut contract = report_value()?;
    contract["schema"] = json!("assay.status/1");
    let mut binding = report_value()?;
    binding["advisory"] = json!(false);

    let refused = [contract, binding].map(|value| {
        serde_json::to_vec(&value)
            .map_err(Box::<dyn Error>::from)
            .and_then(|bytes| refusal(&bytes))
    });

    for error in refused {
        assert!(
            matches!(error?, LocalStatusError::Report(LocalReportError::NotLocal)),
            "refused as not local"
        );
    }
    Ok(())
}

/// `bytes` padded with trailing spaces to `size`.
fn padded(mut bytes: Vec<u8>, size: usize) -> Vec<u8> {
    bytes.resize(size, b' ');
    bytes
}

#[test]
fn a_report_is_read_up_to_the_document_cap() -> Result<(), Box<dyn Error>> {
    let at_cap = padded(report()?, MAX_DOCUMENT_BYTES);
    let over = padded(report()?, MAX_DOCUMENT_BYTES + 1);

    let read = status_of(TREE, Some((TREE, &at_cap)))?;
    let refused = refusal(&over)?;

    assert!(read.is_ok(), "{read:?}");
    assert!(
        matches!(
            refused,
            LocalStatusError::Report(LocalReportError::TooLarge { size }) if size == MAX_DOCUMENT_BYTES + 1
        ),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn findings_are_read_up_to_their_cap() -> Result<(), Box<dyn Error>> {
    let mut value = report_value()?;
    let finding = value["review"]["findings"][0].clone();
    value["gates"][0]["findings"] = json!([]);
    value["review"]["findings"] = Value::Array(vec![finding.clone(); MAX_FINDINGS]);
    let mut over = value.clone();
    over["review"]["findings"] = Value::Array(vec![finding; MAX_FINDINGS + 1]);

    let read = status_of_value(&value)?;
    let refused = status_of_value(&over)?;

    assert!(read.is_ok(), "{read:?}");
    assert!(
        matches!(refused, Err(LocalStatusError::TooManyFindings { count }) if count == MAX_FINDINGS + 1),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn gate_runs_are_read_up_to_their_cap() -> Result<(), Box<dyn Error>> {
    let mut value = report_value()?;
    let mut gate = value["gates"][0].clone();
    gate["findings"] = json!([]);
    value["gates"] = Value::Array(vec![gate.clone(); MAX_CHECK_RUNS]);
    let mut over = value.clone();
    over["gates"] = Value::Array(vec![gate; MAX_CHECK_RUNS + 1]);

    let read = status_of_value(&value)?;
    let refused = status_of_value(&over)?;

    assert!(read.is_ok(), "{read:?}");
    assert!(
        matches!(refused, Err(LocalStatusError::TooManyGates { count }) if count == MAX_CHECK_RUNS + 1),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn reasons_are_read_up_to_their_cap() -> Result<(), Box<dyn Error>> {
    let mut value = report_value()?;
    let reason = value["decision"]["reasons"][0].clone();
    value["decision"]["reasons"] = Value::Array(vec![reason.clone(); MAX_REASONS]);
    let mut over = value.clone();
    over["decision"]["reasons"] = Value::Array(vec![reason; MAX_REASONS + 1]);

    let read = status_of_value(&value)?;
    let refused = status_of_value(&over)?;

    assert!(read.is_ok(), "{read:?}");
    assert!(
        matches!(refused, Err(LocalStatusError::TooManyReasons { count }) if count == MAX_REASONS + 1),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_finding_the_status_cannot_carry_fails_the_schema() -> Result<(), Box<dyn Error>> {
    let mut value = report_value()?;
    value["review"]["findings"][0]["title"] = json!("");

    let refused = status_of_value(&value)?;

    assert!(
        matches!(refused, Err(LocalStatusError::Invalid(_))),
        "{refused:?}"
    );
    Ok(())
}
