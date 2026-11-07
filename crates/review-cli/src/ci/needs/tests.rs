use std::assert_matches;

use super::*;

/// The shape of GitHub's documented "example contents of the `needs` context" (contexts
/// reference), with the docs' job ids replaced by the pipeline's; doc-derived, not recorded
/// from a run.
const DOCS: &[u8] = include_bytes!("../../../tests/fixtures/needs-docs.json");

const GUARD_PLAN: [JobName; 2] = [JobName::Guard, JobName::Plan];

#[test]
fn reads_the_documented_shape() {
    let needs = Needs::parse(DOCS, &GUARD_PLAN).expect("the docs' shape");

    let guard = needs.job(JobName::Guard).expect("guard is needed");
    let plan = needs.job(JobName::Plan).expect("plan is needed");
    assert_eq!(
        (guard.result(), guard.output("pr"), guard.output("run")),
        (JobResult::Success, Some("123456"), Some("true"))
    );
    assert_eq!(
        (plan.result(), plan.outputs().count()),
        (JobResult::Success, 0)
    );
}

#[test]
fn reads_every_result_word() {
    for (word, result) in [
        ("success", JobResult::Success),
        ("failure", JobResult::Failure),
        ("cancelled", JobResult::Cancelled),
        ("skipped", JobResult::Skipped),
    ] {
        let text = format!(r#"{{"guard": {{"result": "{word}", "outputs": {{}}}}}}"#);

        let needs = Needs::parse(text.as_bytes(), &[JobName::Guard]).expect("a known result");

        assert_eq!(
            needs.job(JobName::Guard).map(Upstream::result),
            Some(result),
            "{word}"
        );
    }
}

#[test]
fn reads_every_pipeline_job_id() {
    let text = format!(
        "{{{}}}",
        [
            "guard",
            "open",
            "plan",
            "gates-static",
            "gates-lint",
            "gates-test",
            "admit",
            "review",
            "release",
        ]
        .map(|id| format!(r#""{id}": {{"result": "skipped", "outputs": {{}}}}"#))
        .join(",")
    );

    let needs = Needs::parse(text.as_bytes(), &JobName::ALL).expect("every pipeline job");

    let results: Vec<_> = JobName::ALL
        .iter()
        .map(|&job| needs.job(job).map(Upstream::result))
        .collect();
    assert_eq!(results, [Some(JobResult::Skipped); 9]);
}

#[test]
fn refuses_a_repeated_key() {
    let text = br#"{"guard": {"result": "success", "outputs": {"pr": "1", "pr": "2"}}}"#;

    assert_matches!(
        Needs::parse(text, &[JobName::Guard]),
        Err(NeedsError::Parse(_))
    );
}

#[test]
fn refuses_a_repeated_job() {
    let entry = r#"{"result": "success", "outputs": {}}"#;
    let text = format!(r#"{{"guard": {entry}, "guard": {entry}}}"#);

    assert_matches!(
        Needs::parse(text.as_bytes(), &[JobName::Guard]),
        Err(NeedsError::Parse(_))
    );
}

#[test]
fn refuses_a_job_it_does_not_need() {
    assert_matches!(
        Needs::parse(DOCS, &[JobName::Guard]),
        Err(NeedsError::UnexpectedJob { name }) if name == "plan"
    );
}

#[test]
fn refuses_a_job_that_is_not_a_pipeline_job() {
    let text = br#"{"guard": {"result": "success", "outputs": {}}, "deploy": {"result": "success", "outputs": {}}}"#;

    assert_matches!(
        Needs::parse(text, &[JobName::Guard]),
        Err(NeedsError::UnexpectedJob { name }) if name == "deploy"
    );
}

#[test]
fn an_unexpected_job_id_is_shown_cut_to_64_bytes() {
    let id = "é".repeat(40);
    let text = format!(r#"{{"{id}": {{"result": "success", "outputs": {{}}}}}}"#);

    assert_matches!(
        Needs::parse(text.as_bytes(), &[]),
        Err(NeedsError::UnexpectedJob { name }) if name == "é".repeat(32)
    );
}

#[test]
fn refuses_a_missing_needed_job() {
    let text = br#"{"guard": {"result": "success", "outputs": {}}}"#;

    assert_matches!(
        Needs::parse(text, &GUARD_PLAN),
        Err(NeedsError::MissingJob { job: JobName::Plan })
    );
}

#[test]
fn refuses_an_unknown_result() {
    for result in [r#""neutral""#, r#""Success""#, "null", "1"] {
        let text = format!(r#"{{"guard": {{"result": {result}, "outputs": {{}}}}}}"#);

        assert_matches!(
            Needs::parse(text.as_bytes(), &[JobName::Guard]),
            Err(NeedsError::Result {
                job: JobName::Guard
            }),
            "{result}"
        );
    }
}

#[test]
fn refuses_a_non_string_output() {
    for value in ["1", "true", "null", "[]", "{}"] {
        let text = format!(r#"{{"guard": {{"result": "success", "outputs": {{"pr": {value}}}}}}}"#);

        assert_matches!(
            Needs::parse(text.as_bytes(), &[JobName::Guard]),
            Err(NeedsError::NotString { job: JobName::Guard, name }) if name == "pr",
            "{value}"
        );
    }
}

#[test]
fn refuses_an_entry_of_another_shape() {
    let cases = [
        ("[]", EntryProblem::NotAnObject),
        (r#"{"outputs": {}}"#, EntryProblem::MissingResult),
        (r#"{"result": "success"}"#, EntryProblem::MissingOutputs),
        (
            r#"{"result": "success", "outputs": []}"#,
            EntryProblem::OutputsNotAnObject,
        ),
        (
            r#"{"result": "success", "outputs": {}, "conclusion": "success"}"#,
            EntryProblem::UnknownKey("conclusion".to_owned()),
        ),
    ];
    for (entry, expected) in cases {
        let text = format!(r#"{{"guard": {entry}}}"#);

        assert_matches!(
            Needs::parse(text.as_bytes(), &[JobName::Guard]),
            Err(NeedsError::Entry { job: JobName::Guard, problem }) if problem == expected,
            "{entry}"
        );
    }
}

#[test]
fn refuses_a_value_that_is_not_an_object() {
    for text in ["[]", "null", r#""guard""#] {
        assert_matches!(
            Needs::parse(text.as_bytes(), &[]),
            Err(NeedsError::NotAnObject),
            "{text}"
        );
    }
}

#[test]
fn reads_up_to_the_cap_and_refuses_one_byte_more_unparsed() {
    let base = br#"{"guard": {"result": "success", "outputs": {}}}"#;
    let mut at_cap = base.to_vec();
    at_cap.resize(MAX_NEEDS_BYTES, b' ');
    let mut over = at_cap.clone();
    over.push(b' ');

    let read = Needs::parse(&at_cap, &[JobName::Guard])
        .map(|needs| needs.job(JobName::Guard).map(Upstream::result));

    assert_matches!(read, Ok(Some(JobResult::Success)));
    assert_matches!(
        Needs::parse(&over, &[JobName::Guard]),
        Err(NeedsError::TooLarge { size: 262_145 })
    );
}
