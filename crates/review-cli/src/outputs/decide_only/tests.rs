use std::assert_matches;

use super::*;
use crate::ci::Outputs;

/// `n` (at least 1) as a PR number.
fn pr(n: u64) -> NonZeroU64 {
    NonZeroU64::MIN.saturating_add(n.saturating_sub(1))
}

/// `toJSON(needs)` for a `guard` that ended with `result` and wrote `outputs`.
fn needs_text(result: &str, outputs: &[(&str, &str)]) -> String {
    let outputs: serde_json::Map<String, serde_json::Value> = outputs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::json!({ "guard": { "result": result, "outputs": outputs } }).to_string()
}

fn decoded_after(
    result: &str,
    outputs: &[(&str, &str)],
) -> Result<DecideOnlyGuardOutputs, NeedsError> {
    let needs = Needs::parse(needs_text(result, outputs).as_bytes(), &[JobName::Guard])?;
    DecideOnlyGuardOutputs::from_needs(&needs)
}

fn decoded(outputs: &[(&str, &str)]) -> Result<DecideOnlyGuardOutputs, NeedsError> {
    decoded_after("success", outputs)
}

/// The bytes `outputs` appends to `GITHUB_OUTPUT`.
fn written(outputs: &[(OutputName, String)]) -> String {
    let mut file = Vec::new();
    let mut writer = Outputs::new(&mut file);
    for (name, value) in outputs {
        writer.write(*name, value).expect("single-line values");
    }
    String::from_utf8(file).expect("UTF-8 outputs")
}

#[test]
fn one_pr_from_a_comment_writes_prs_then_trigger() {
    let prs = DecideOnlyPrs::try_from(vec![pr(2)]).expect("one PR");

    let text = written(&DecideOnlyGuardOutputs::new(prs, DecideTrigger::Comment).outputs());

    assert_eq!(text, "prs=[2]\ntrigger=comment\n");
}

#[test]
fn a_skip_writes_an_empty_prs_alone() {
    let text = written(&DECIDE_ONLY_SKIP.map(|(name, value)| (name, value.to_owned())));

    assert_eq!(text, "prs=[]\n");
}

#[test]
fn each_trigger_round_trips_through_needs() {
    for (trigger, word) in [
        (DecideTrigger::Comment, "comment"),
        (DecideTrigger::Label, "label"),
        (DecideTrigger::Schedule, "schedule"),
    ] {
        let prs = DecideOnlyPrs::try_from(vec![pr(277), pr(3), pr(41)]).expect("three PRs");
        let outputs = DecideOnlyGuardOutputs::new(prs, trigger);

        let pairs = outputs.outputs();
        let read = decoded(
            &pairs
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
        );

        assert_eq!(
            pairs.get(1).map(|(_, value)| value.as_str()),
            Some(word),
            "{trigger:?}"
        );
        assert_eq!(read.ok(), Some(outputs), "{trigger:?}");
    }
}

#[test]
fn the_prs_keep_their_order() {
    let read = decoded(&[("prs", "[7,2,30]"), ("trigger", "schedule")]).expect("three PRs");

    assert_eq!(read.prs().as_slice(), [pr(7), pr(2), pr(30)]);
    assert_eq!(read.trigger(), DecideTrigger::Schedule);
}

#[test]
fn a_pr_set_is_refused_empty_repeated_or_over_the_cap() {
    let over: Vec<NonZeroU64> = (1..=21).map(pr).collect();
    let at_cap: Vec<NonZeroU64> = (1..=20).map(pr).collect();

    assert_eq!(DecideOnlyPrs::try_from(Vec::new()), Err(PrsError::Empty));
    assert_eq!(
        DecideOnlyPrs::try_from(vec![pr(2), pr(3), pr(2)]),
        Err(PrsError::Repeated { pr: pr(2) })
    );
    assert_eq!(
        DecideOnlyPrs::try_from(over),
        Err(PrsError::OverCap { count: 21 })
    );
    assert_eq!(
        DecideOnlyPrs::try_from(at_cap.clone()).map(|prs| prs.as_slice().to_vec()),
        Ok(at_cap)
    );
}

#[test]
fn prs_written_other_than_exactly_as_guard_writes_them_are_refused() {
    let over = format!(
        "[{}]",
        (1..=21)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    for prs in [
        "[]",
        "[2, 3]",
        " [2]",
        "[02]",
        "[0]",
        "[2,]",
        "[,2]",
        "2",
        "[2,2]",
        "[-2]",
        "[2.0]",
        over.as_str(),
    ] {
        let read = decoded(&[("prs", prs), ("trigger", "comment")]);

        assert_matches!(
            read,
            Err(NeedsError::Output {
                job: JobName::Guard,
                name: OutputName::Prs
            }),
            "{prs:?}"
        );
    }
}

#[test]
fn a_trigger_other_than_decide_only_s_is_refused() {
    for trigger in ["opened", "Comment", "comment ", "workflow_dispatch"] {
        let read = decoded(&[("prs", "[2]"), ("trigger", trigger)]);

        assert_matches!(
            read,
            Err(NeedsError::Output {
                job: JobName::Guard,
                name: OutputName::Trigger
            }),
            "{trigger:?}"
        );
    }
}

#[test]
fn each_output_is_required_and_no_other_is_read() {
    assert_matches!(
        decoded(&[("trigger", "comment")]),
        Err(NeedsError::MissingOutput {
            job: JobName::Guard,
            name: OutputName::Prs
        })
    );
    assert_matches!(
        decoded(&[("prs", "[2]")]),
        Err(NeedsError::MissingOutput {
            job: JobName::Guard,
            name: OutputName::Trigger
        })
    );
    assert_matches!(
        decoded(&[("prs", "[2]"), ("trigger", "comment"), ("run", "true")]),
        Err(NeedsError::UnexpectedOutput { job: JobName::Guard, name }) if name == "run"
    );
}

#[test]
fn a_guard_that_did_not_succeed_is_refused() {
    let read = decoded_after("failure", &[("prs", "[2]"), ("trigger", "comment")]);

    assert_matches!(
        read,
        Err(NeedsError::NotSucceeded {
            job: JobName::Guard,
            ..
        })
    );
}

#[test]
fn a_redecision_bundle_writes_its_digest_and_artifact_name() {
    let digest = Sha256Digest::of(b"run-decision");
    let nonce: ArtifactNonce = format!("{:032x}", u128::default())
        .parse()
        .expect("a nonce");

    let outputs = RedecideOutputs::bundle(pr(352), digest.clone(), &nonce);

    assert_eq!(
        written(&outputs.outputs()),
        format!(
            "bundle=true\nbundle_digest={digest}\n\
             artifact=assay-decision-352-00000000000000000000000000000000\n"
        )
    );
}

#[test]
fn no_redecision_bundle_writes_bundle_false_alone() {
    assert_eq!(
        written(&RedecideOutputs::NoBundle.outputs()),
        "bundle=false\n"
    );
}
