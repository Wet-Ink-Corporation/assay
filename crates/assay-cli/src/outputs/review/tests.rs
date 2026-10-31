use std::assert_matches;

use super::*;
use crate::ci::Outputs;

/// `toJSON(needs)` for a `review` that ended with `result` and wrote `outputs`.
fn needs_text(result: &str, outputs: &[(&str, &str)]) -> String {
    let outputs: serde_json::Map<String, serde_json::Value> = outputs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::json!({ "review": { "result": result, "outputs": outputs } }).to_string()
}

fn decoded_after(result: &str, outputs: &[(&str, &str)]) -> Result<ReviewEnd, NeedsError> {
    let needs = Needs::parse(needs_text(result, outputs).as_bytes(), &[JobName::Review])?;
    ReviewEnd::from_needs(&needs)
}

fn decoded(outputs: &[(&str, &str)]) -> Result<ReviewEnd, NeedsError> {
    decoded_after("success", outputs)
}

const PLACEHOLDER: [(&str, &str); 3] =
    [("stage", "not-run"), ("cost_aeu", "0"), ("documents", "[]")];

#[test]
fn the_placeholder_round_trips_to_a_review_that_did_not_run_and_spent_nothing() {
    let mut file = Vec::new();
    let mut outputs = Outputs::new(&mut file);
    for (name, value) in REVIEW_PLACEHOLDER {
        outputs.write(name, value).expect("single-line values");
    }
    let text = String::from_utf8(file).expect("UTF-8 outputs");
    let pairs: Vec<(&str, &str)> = text
        .lines()
        .map(|line| line.split_once('=').expect("name=value"))
        .collect();

    let read = decoded(&pairs);

    assert_eq!(pairs, PLACEHOLDER);
    assert_eq!(
        read.ok(),
        Some(ReviewEnd::Reported(ReviewReport {
            stage: ReviewStage::NotRun,
            cost: Aeu::ZERO,
        }))
    );
}

#[test]
fn a_reported_cost_is_read_in_the_ledger_s_form() {
    let read = decoded(&[
        ("stage", "not-run"),
        ("cost_aeu", "0.512"),
        ("documents", "[]"),
    ]);

    assert_eq!(
        read.ok(),
        Some(ReviewEnd::Reported(ReviewReport {
            stage: ReviewStage::NotRun,
            cost: Aeu::from_micros(512_000),
        }))
    );
}

#[test]
fn a_review_that_did_not_succeed_is_read_without_its_outputs() {
    for (result, end) in [
        ("failure", ReviewEnd::Failed),
        ("cancelled", ReviewEnd::Cancelled),
        ("skipped", ReviewEnd::Skipped),
    ] {
        let read = decoded_after(result, &[("stage", "garbage"), ("other", "x")]);

        assert_eq!(read.ok(), Some(end), "{result}");
    }
}

#[test]
fn a_stage_other_than_not_run_is_refused() {
    for stage in ["completed", "Not-Run", "not-run ", "notrun"] {
        let read = decoded(&[("stage", stage), ("cost_aeu", "0"), ("documents", "[]")]);

        assert_matches!(
            read,
            Err(NeedsError::Output {
                job: JobName::Review,
                name: OutputName::Stage
            }),
            "{stage:?}"
        );
    }
}

#[test]
fn a_cost_not_in_the_ledger_s_form_is_refused() {
    for cost in ["0.0", "00", "-1", "1e3", "0.5000000"] {
        let read = decoded(&[
            ("stage", "not-run"),
            ("cost_aeu", cost),
            ("documents", "[]"),
        ]);

        assert_matches!(
            read,
            Err(NeedsError::Output {
                job: JobName::Review,
                name: OutputName::CostAeu
            }),
            "{cost:?}"
        );
    }
}

#[test]
fn documents_other_than_none_are_refused() {
    for documents in ["[ ]", r#"["x"]"#, "{}", "null"] {
        let read = decoded(&[
            ("stage", "not-run"),
            ("cost_aeu", "0"),
            ("documents", documents),
        ]);

        assert_matches!(
            read,
            Err(NeedsError::Output {
                job: JobName::Review,
                name: OutputName::Documents
            }),
            "{documents:?}"
        );
    }
}

#[test]
fn every_review_output_is_required() {
    for missing in OutputName::REVIEW {
        let outputs: Vec<(&str, &str)> = PLACEHOLDER
            .into_iter()
            .filter(|&(name, _)| name != missing.as_str())
            .collect();

        let read = decoded(&outputs);

        assert_matches!(
            read,
            Err(NeedsError::MissingOutput { job: JobName::Review, name }) if name == missing,
            "{missing}"
        );
    }
}

#[test]
fn an_output_that_is_not_the_review_s_is_refused() {
    let mut outputs = PLACEHOLDER.to_vec();
    outputs.push(("slot", "0"));

    let read = decoded(&outputs);

    assert_matches!(
        read,
        Err(NeedsError::UnexpectedOutput { job: JobName::Review, name }) if name == "slot"
    );
}

#[test]
fn a_missing_review_is_refused() {
    let needs = Needs::parse(b"{}", &[]).expect("an empty needs");

    assert_matches!(
        ReviewEnd::from_needs(&needs),
        Err(NeedsError::MissingJob {
            job: JobName::Review
        })
    );
}

#[test]
fn the_stage_reads_back_from_its_word_and_nothing_else_parses() {
    assert_eq!(
        ReviewStage::NotRun.as_str().parse::<ReviewStage>(),
        Ok(ReviewStage::NotRun)
    );
    assert_eq!("".parse::<ReviewStage>(), Err(UnknownReviewStage));
    assert_eq!("completed".parse::<ReviewStage>(), Err(UnknownReviewStage));
}
