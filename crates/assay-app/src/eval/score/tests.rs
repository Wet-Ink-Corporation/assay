use std::num::NonZeroU64;

use assay_domain::Class;
use serde_json::{Value, json};

use super::{ClassMap, MAX_RUN_FINDINGS, OutputProblem, ReportedFinding, matches, read_findings};
use crate::eval::case::Label;

fn line(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("test lines are positive")
}

/// The fixing hunk every scorer test aims at: lines 40 to 44 of `src/lib.rs`, a logic defect.
fn hunk() -> Label {
    Label::new("src/lib.rs", line(40), line(44), Class::Logic).expect("a valid label")
}

fn finding(file: &str, first: u64, last: u64, class: Class) -> ReportedFinding {
    ReportedFinding {
        file: file.to_owned(),
        first: line(first),
        last: line(last),
        class,
    }
}

#[test]
fn a_finding_inside_the_hunk_in_the_same_file_and_class_matches() {
    let found = finding("src/lib.rs", 42, 42, Class::Logic);

    assert!(matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn a_finding_ten_lines_before_the_hunk_matches() {
    let found = finding("src/lib.rs", 30, 30, Class::Logic);

    assert!(matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn a_finding_eleven_lines_before_the_hunk_misses() {
    let found = finding("src/lib.rs", 29, 29, Class::Logic);

    assert!(!matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn a_finding_ten_lines_after_the_hunk_matches() {
    let found = finding("src/lib.rs", 54, 54, Class::Logic);

    assert!(matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn a_finding_eleven_lines_after_the_hunk_misses() {
    let found = finding("src/lib.rs", 55, 55, Class::Logic);

    assert!(!matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn a_finding_spanning_into_the_window_from_outside_it_matches() {
    let found = finding("src/lib.rs", 1, 30, Class::Logic);

    assert!(matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn the_window_stops_at_line_one_near_the_top_of_a_file() {
    let top = Label::new("src/lib.rs", line(3), line(3), Class::Logic).expect("a valid label");
    let found = finding("src/lib.rs", 1, 1, Class::Logic);

    assert!(matches(&top, &found, &ClassMap::identity()));
}

#[test]
fn a_finding_in_another_file_misses() {
    let found = finding("src/main.rs", 42, 42, Class::Logic);

    assert!(!matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn a_finding_of_another_class_misses_under_the_identity_map() {
    let found = finding("src/lib.rs", 42, 42, Class::ErrorHandling);

    assert!(!matches(&hunk(), &found, &ClassMap::identity()));
}

#[test]
fn a_finding_of_an_aliased_class_matches() {
    let map = ClassMap::identity().with_alias(Class::Logic, Class::ErrorHandling);
    let found = finding("src/lib.rs", 42, 42, Class::ErrorHandling);

    assert!(matches(&hunk(), &found, &map));
}

#[test]
fn an_alias_maps_one_way_only() {
    let map = ClassMap::identity().with_alias(Class::ErrorHandling, Class::Logic);
    let found = finding("src/lib.rs", 42, 42, Class::ErrorHandling);

    assert!(!matches(&hunk(), &found, &map));
}

fn output(findings: &[Value]) -> Value {
    json!({ "findings": findings })
}

fn reported(index: u64) -> Value {
    json!({
        "file": "src/lib.rs",
        "line": index + 1,
        "class": "logic",
        "title": "ignored by the scorer",
        "severity": "major",
    })
}

#[test]
fn reads_file_lines_and_class_of_each_finding() {
    let value = output(&[
        json!({"file": "a.rs", "line": 3, "class": "logic"}),
        json!({"file": "b.rs", "line": 5, "end_line": 9, "class": "security", "body": "x"}),
    ]);

    let findings = read_findings(&value);

    assert_eq!(
        findings,
        Ok(vec![
            finding("a.rs", 3, 3, Class::Logic),
            finding("b.rs", 5, 9, Class::Security),
        ])
    );
}

#[test]
fn refuses_output_that_is_not_an_object_with_a_findings_list() {
    for value in [json!([]), json!({}), json!({"findings": {}}), json!("text")] {
        assert_eq!(
            read_findings(&value),
            Err(OutputProblem::NotADocument),
            "{value}"
        );
    }
}

#[test]
fn refuses_a_finding_without_a_positive_line() {
    let value = output(&[
        reported(0),
        json!({"file": "a.rs", "line": 0, "class": "logic"}),
    ]);

    assert_eq!(
        read_findings(&value),
        Err(OutputProblem::Finding { index: 1 })
    );
}

#[test]
fn refuses_a_finding_that_ends_before_it_starts() {
    let value = output(&[json!({"file": "a.rs", "line": 9, "end_line": 8, "class": "logic"})]);

    assert_eq!(
        read_findings(&value),
        Err(OutputProblem::Finding { index: 0 })
    );
}

#[test]
fn refuses_a_finding_of_an_unknown_class() {
    let value = output(&[json!({"file": "a.rs", "line": 9, "class": "style"})]);

    assert_eq!(
        read_findings(&value),
        Err(OutputProblem::Finding { index: 0 })
    );
}

#[test]
fn reads_as_many_findings_as_the_cap() {
    let cap = u64::try_from(MAX_RUN_FINDINGS).expect("the cap fits u64");
    let value = output(&(0..cap).map(reported).collect::<Vec<_>>());

    let findings = read_findings(&value).expect("the cap is allowed");

    assert_eq!(findings.len(), MAX_RUN_FINDINGS);
}

#[test]
fn refuses_one_finding_past_the_cap() {
    let over = u64::try_from(MAX_RUN_FINDINGS).expect("the cap fits u64") + 1;
    let value = output(&(0..over).map(reported).collect::<Vec<_>>());

    assert_eq!(read_findings(&value), Err(OutputProblem::TooManyFindings));
}
