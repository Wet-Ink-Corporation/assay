use std::assert_matches;

use proptest::prelude::*;

use super::*;
use crate::ci::{JobResult, Outputs};

const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const KEY: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const MERGE_BASE: &str = "3333333333333333333333333333333333333333";

/// `toJSON(needs)` for a `plan` that ended with `result` and wrote `outputs`.
fn needs_text(result: &str, outputs: &[(&str, &str)]) -> String {
    let outputs: serde_json::Map<String, serde_json::Value> = outputs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::json!({ "plan": { "result": result, "outputs": outputs } }).to_string()
}

fn decoded(outputs: &[(&str, &str)]) -> Result<PlanOutputs, NeedsError> {
    let needs = Needs::parse(needs_text("success", outputs).as_bytes(), &[JobName::Plan])?;
    PlanOutputs::from_needs(&needs)
}

/// The names of the seven artifacts, nonce `n` for job `n`.
fn artifact_names() -> Vec<String> {
    ArtifactJob::ALL
        .into_iter()
        .zip(1_u8..)
        .map(|(job, n)| format!("assay-{}-{}", job.name(), format!("{n:x}").repeat(32)))
        .collect()
}

/// A ready `full` run's documented outputs, in [`OutputName::PLAN`]'s order.
fn documented() -> Vec<(&'static str, String)> {
    let facts = [
        ("run_digest", DIGEST),
        ("run_key", KEY),
        ("tier", "full"),
        ("draft", "false"),
        ("review", "true"),
        ("merge_base", MERGE_BASE),
    ]
    .map(|(name, value)| (name, value.to_owned()));
    let artifacts = ArtifactJob::ALL
        .into_iter()
        .map(|job| OutputName::artifact(job).as_str())
        .zip(artifact_names());
    facts.into_iter().chain(artifacts).collect()
}

fn borrowed<'o>(outputs: &'o [(&'static str, String)]) -> Vec<(&'static str, &'o str)> {
    outputs
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

/// `outputs` with `name` set to `value`.
fn with(name: &str, value: &str) -> Vec<(&'static str, String)> {
    documented()
        .into_iter()
        .map(|(own, old)| (own, if own == name { value.to_owned() } else { old }))
        .collect()
}

/// What `outputs()` wrote, as the workflow reads it.
fn written(outputs: &PlanOutputs) -> Vec<(&'static str, String)> {
    outputs
        .outputs()
        .into_iter()
        .map(|(name, value)| (name.as_str(), value))
        .collect()
}

#[test]
fn decodes_the_documented_outputs_and_writes_them_back() {
    let read = decoded(&borrowed(&documented())).expect("plan's documented outputs");

    assert_eq!(
        (
            read.run_digest().as_str(),
            read.run_key().as_str(),
            read.tier(),
            read.draft(),
            read.review(),
            read.merge_base().as_str(),
        ),
        (DIGEST, KEY, Tier::Full, false, true, MERGE_BASE)
    );
    assert_eq!(written(&read), documented());
}

#[test]
fn every_output_passes_through_the_output_writer() {
    let read = decoded(&borrowed(&documented())).expect("plan's documented outputs");
    let mut file = Vec::new();
    let mut outputs = Outputs::new(&mut file);

    for (name, value) in read.outputs() {
        outputs.write(name, &value).expect("single-line values");
    }

    let expected: String = documented()
        .iter()
        .flat_map(|(name, value)| [*name, "=", value.as_str(), "\n"])
        .collect();
    assert_eq!(String::from_utf8(file).expect("UTF-8 outputs"), expected);
}

#[test]
fn a_review_that_contradicts_the_tier_or_draft_is_refused() {
    let cases = [
        with("draft", "true"),
        with("review", "false"),
        with("tier", "trivial"),
        with("tier", "needs-split"),
    ];

    for outputs in cases {
        assert_matches!(
            decoded(&borrowed(&outputs)),
            Err(NeedsError::Output {
                job: JobName::Plan,
                name: OutputName::Review
            }),
            "{outputs:?}"
        );
    }
}

#[test]
fn a_draft_or_a_tier_without_an_llm_stage_reads_as_not_reviewed() {
    let draft: Vec<_> = with("draft", "true")
        .into_iter()
        .map(|(name, value)| match name {
            "review" => (name, "false".to_owned()),
            _ => (name, value),
        })
        .collect();

    let read = decoded(&borrowed(&draft)).expect("a draft's outputs");

    assert_eq!((read.draft(), read.review()), (true, false));
}

#[test]
fn an_artifact_name_of_another_job_or_without_a_nonce_is_refused() {
    let nonce = "a".repeat(32);
    for value in [
        format!("assay-review-{nonce}"),
        "assay-admit-xyz".to_owned(),
        format!("assay-admit-{}", "A".repeat(32)),
        format!("admit-{nonce}"),
        format!("assay-admit-{nonce}-"),
    ] {
        assert_matches!(
            decoded(&borrowed(&with("artifact_admit", &value))),
            Err(NeedsError::Output {
                job: JobName::Plan,
                name: OutputName::ArtifactAdmit
            }),
            "{value}"
        );
    }
}

#[test]
fn a_value_that_does_not_parse_is_refused_by_name() {
    let cases = [
        ("run_digest", "sha256:abc", OutputName::RunDigest),
        (
            "run_key",
            KEY.trim_start_matches("sha256:"),
            OutputName::RunKey,
        ),
        ("tier", "Full", OutputName::Tier),
        ("draft", "no", OutputName::Draft),
        ("review", "1", OutputName::Review),
        ("merge_base", "3333", OutputName::MergeBase),
    ];

    for (name, value, refused) in cases {
        assert_matches!(
            decoded(&borrowed(&with(name, value))),
            Err(NeedsError::Output { job: JobName::Plan, name }) if name == refused,
            "{name}={value}"
        );
    }
}

#[test]
fn each_missing_or_empty_output_is_refused_by_name() {
    for name in OutputName::PLAN {
        let absent: Vec<_> = documented()
            .into_iter()
            .filter(|&(own, _)| own != name.as_str())
            .collect();
        let empty = with(name.as_str(), "");

        for outputs in [absent, empty] {
            assert_matches!(
                decoded(&borrowed(&outputs)),
                Err(NeedsError::MissingOutput { job: JobName::Plan, name: missing })
                    if missing == name,
                "{name}"
            );
        }
    }
}

#[test]
fn an_output_plan_does_not_write_is_refused() {
    let mut outputs = documented();
    outputs.push(("run", "true".to_owned()));

    assert_matches!(
        decoded(&borrowed(&outputs)),
        Err(NeedsError::UnexpectedOutput { job: JobName::Plan, name }) if name == "run"
    );
}

#[test]
fn a_plan_that_did_not_succeed_is_refused() {
    let outputs = documented();
    let needs = Needs::parse(
        needs_text("failure", &borrowed(&outputs)).as_bytes(),
        &[JobName::Plan],
    )
    .expect("plan's needs");

    assert_matches!(
        PlanOutputs::from_needs(&needs),
        Err(NeedsError::NotSucceeded {
            job: JobName::Plan,
            result: JobResult::Failure
        })
    );
}

/// 32 lowercase hex digits.
fn nonces() -> impl Strategy<Value = String> {
    "[0-9a-f]{32}"
}

/// Any outputs `plan` can write: `review` follows the tier and the draft.
fn valid_outputs() -> impl Strategy<Value = Vec<(&'static str, String)>> {
    (
        "[0-9a-f]{64}",
        "[0-9a-f]{64}",
        prop::sample::select(Tier::ALL.to_vec()),
        any::<bool>(),
        "[0-9a-f]{40}",
        prop::collection::vec(nonces(), ArtifactJob::ALL.len()),
    )
        .prop_map(|(digest, key, tier, draft, merge_base, nonces)| {
            let facts = [
                ("run_digest", format!("sha256:{digest}")),
                ("run_key", format!("sha256:{key}")),
                ("tier", tier.code().to_owned()),
                ("draft", flag_word(draft).to_owned()),
                ("review", flag_word(reviewed(tier, draft)).to_owned()),
                ("merge_base", merge_base),
            ];
            let artifacts = ArtifactJob::ALL
                .into_iter()
                .zip(nonces)
                .map(|(job, nonce)| {
                    (
                        OutputName::artifact(job).as_str(),
                        format!("assay-{}-{nonce}", job.name()),
                    )
                });
            facts.into_iter().chain(artifacts).collect()
        })
}

proptest! {
    #[test]
    fn encode_inverts_decode(outputs in valid_outputs()) {
        let read = decoded(&borrowed(&outputs));

        prop_assert_eq!(read.map(|read| written(&read)).ok(), Some(outputs));
    }
}
