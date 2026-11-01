use std::assert_matches;

use serde_json::{Map, Value, json};

use super::*;

const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const KEY: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const COMMIT: &str = "3333333333333333333333333333333333333333";

/// A job's needs entry: `result` and `outputs`.
fn entry(result: &str, outputs: &[(&str, &str)]) -> Value {
    let outputs: Map<String, Value> = outputs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.into()))
        .collect();
    json!({ "result": result, "outputs": outputs })
}

/// guard's outputs for a ready PR.
fn guard() -> Value {
    entry(
        "success",
        &[
            ("run", "true"),
            ("pr", "42"),
            ("head_sha", COMMIT),
            ("base_sha", COMMIT),
            ("base_ref", "main"),
            ("draft", "false"),
            ("author_id", "7"),
            ("sender_id", "8"),
            ("trigger", "opened"),
            ("default_branch", "main"),
        ],
    )
}

/// plan's outputs for a run of `tier`, a draft or not.
fn plan(tier: &str, draft: bool) -> Value {
    let reviews = !draft && !matches!(tier, "trivial" | "needs-split");
    let mut outputs: Vec<(String, String)> = [
        ("run_digest", DIGEST),
        ("run_key", KEY),
        ("tier", tier),
        ("draft", flag_word(draft)),
        ("review", flag_word(reviews)),
        ("merge_base", COMMIT),
    ]
    .iter()
    .map(|&(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    for (job, n) in assay_app::ArtifactJob::ALL.into_iter().zip(1_u8..) {
        let name = format!("assay-{}-{}", job.name(), format!("{n:x}").repeat(32));
        outputs.push((OutputName::artifact(job).as_str().to_owned(), name));
    }
    let pairs: Vec<(&str, &str)> = outputs
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    entry("success", &pairs)
}

/// Every job decide needs: guard and `plan` as given, the gates jobs reporting nothing, admit
/// as given, the placeholder review, and release.
fn needs(plan: &Value, admit: &Value) -> Result<Needs, NeedsError> {
    let gates = entry("success", &[("gates", "[]")]);
    let text = json!({
        "guard": guard(),
        "open": entry("success", &[]),
        "plan": plan,
        "gates-static": gates,
        "gates-lint": gates,
        "gates-test": gates,
        "admit": admit,
        "review": entry("success", &[("stage", "not-run"), ("cost_aeu", "0"), ("documents", "[]")]),
        "release": entry("success", &[]),
    })
    .to_string();
    Needs::parse(text.as_bytes(), &DECIDE_NEEDS)
}

/// What decide reads of an admit that ended with `result` and wrote `outputs`, after a `full`
/// ready plan.
fn admitted(result: &str, outputs: &[(&str, &str)]) -> Result<AdmitJob, NeedsError> {
    let needs = needs(&plan("full", false), &entry(result, outputs))?;
    let plan = PlanOutputs::from_needs(&needs)?;
    admit_job(&needs, Some(&plan))
}

#[test]
fn no_bundle_writes_bundle_false_alone() {
    assert_eq!(
        DecideOutputs::NoBundle.outputs(),
        [(OutputName::Bundle, "false".to_owned())]
    );
}

#[test]
fn a_bundle_writes_its_digest_after_bundle_true() {
    let digest: Sha256Digest = DIGEST.parse().expect("a digest");

    let outputs = DecideOutputs::Bundle { digest }.outputs();

    assert_eq!(
        outputs,
        [
            (OutputName::Bundle, "true".to_owned()),
            (OutputName::BundleDigest, DIGEST.to_owned()),
        ]
    );
}

#[test]
fn a_spend_is_read_at_the_tier_admit_reports_with_its_degrade() {
    let admit = admitted(
        "success",
        &[
            ("admission", "spend"),
            ("slot", "0"),
            ("lease_commit", COMMIT),
            ("reserved_aeu", "4"),
            ("tier", "lite"),
            ("degrade", "full"),
            ("run_digest", DIGEST),
            ("pack_digest", KEY),
            ("review", "true"),
        ],
    );

    assert_eq!(
        admit.ok(),
        Some(AdmitJob {
            result: assay_app::JobResult::Success,
            degrade: TierDegrade::FullToLite,
            admission: AdmitOutcome::Spend { tier: Tier::Lite },
        })
    );
}

#[test]
fn a_deferral_is_read_with_its_code() {
    for (code, deferred) in [
        ("budget-deferred", Deferred::Budget),
        ("slot-busy", Deferred::SlotBusy),
    ] {
        let admit = admitted(
            "success",
            &[
                ("admission", "deferred"),
                ("deferral", code),
                ("reserved_aeu", "0"),
                ("tier", "full"),
                ("degrade", "none"),
                ("run_digest", DIGEST),
                ("review", "false"),
            ],
        );

        assert_eq!(
            admit.map(|admit| admit.admission).ok(),
            Some(AdmitOutcome::Deferred(deferred)),
            "{code}"
        );
    }
}

#[test]
fn no_spend_is_read_as_the_plan_s_reason() -> Result<(), NeedsError> {
    let no_spend = entry(
        "success",
        &[
            ("admission", "no-spend"),
            ("reserved_aeu", "0"),
            ("tier", "full"),
            ("degrade", "none"),
            ("run_digest", DIGEST),
            ("review", "false"),
        ],
    );
    for (tier, draft, reason) in [
        ("full", true, NotRunReason::Draft),
        ("trivial", false, NotRunReason::NoLlmTier),
    ] {
        let needs = needs(&plan(tier, draft), &no_spend)?;
        let plan = PlanOutputs::from_needs(&needs)?;

        let admit = admit_job(&needs, Some(&plan))?;

        assert_eq!(admit.admission, AdmitOutcome::NoSpend(reason), "{tier}");
    }
    Ok(())
}

#[test]
fn an_admission_the_plan_does_not_allow_is_refused() {
    for outputs in [
        // A ready `full` plan has a review: no-spend contradicts it.
        &[
            ("admission", "no-spend"),
            ("degrade", "none"),
            ("review", "false"),
        ][..],
        &[
            ("admission", "spend"),
            ("tier", "full"),
            ("degrade", "none"),
            ("review", "false"),
        ],
        &[
            ("admission", "deferred"),
            ("deferral", "later"),
            ("degrade", "none"),
            ("review", "false"),
        ],
        &[
            ("admission", "maybe"),
            ("degrade", "none"),
            ("review", "false"),
        ],
    ] {
        assert_matches!(
            admitted("success", outputs),
            Err(NeedsError::Output {
                job: JobName::Admit,
                ..
            }),
            "{outputs:?}"
        );
    }
}

#[test]
fn a_deferral_beside_another_admission_is_refused() {
    let read = admitted(
        "success",
        &[
            ("admission", "reused"),
            ("deferral", "slot-busy"),
            ("degrade", "none"),
            ("review", "false"),
        ],
    );

    assert_matches!(
        read,
        Err(NeedsError::Output {
            job: JobName::Admit,
            name: OutputName::Deferral
        })
    );
}

#[test]
fn an_admit_that_did_not_succeed_is_passed_on_with_no_degrade() {
    for (result, expected) in [
        ("failure", assay_app::JobResult::Failure),
        ("cancelled", assay_app::JobResult::Cancelled),
        ("skipped", assay_app::JobResult::Skipped),
    ] {
        let admit = admitted(
            result,
            &[("slot", "1"), ("lease_commit", COMMIT), ("degrade", "full")],
        );

        assert_matches!(
            admit,
            Ok(AdmitJob { result, degrade: TierDegrade::None, .. }) if result == expected
        );
    }
}

#[test]
fn a_plan_that_did_not_succeed_is_read_by_its_result_and_skipped_jobs_report_nothing()
-> Result<(), NeedsError> {
    let skipped = entry("skipped", &[]);
    let text = json!({
        "guard": guard(),
        "open": entry("success", &[]),
        "plan": entry("failure", &[]),
        "gates-static": skipped,
        "gates-lint": skipped,
        "gates-test": skipped,
        "admit": skipped,
        "review": skipped,
        "release": skipped,
    })
    .to_string();
    let needs = Needs::parse(text.as_bytes(), &DECIDE_NEEDS)?;

    let read = DecideNeeds::from_needs(&needs)?;

    assert_eq!(read.plan, PlanEnd::NotSucceeded(JobResult::Failure));
    assert_eq!(read.gates_static.result, assay_app::JobResult::Skipped);
    assert!(read.gates_static.gates.is_empty());
    assert_eq!(read.admit.result, assay_app::JobResult::Skipped);
    assert_eq!(read.review.result, assay_app::JobResult::Skipped);
    assert_eq!(read.release, assay_app::JobResult::Skipped);
    Ok(())
}

#[test]
fn the_placeholder_review_is_read_as_not_run_at_no_cost() -> Result<(), NeedsError> {
    let needs = needs(&plan("full", false), &entry("skipped", &[]))?;

    let read = DecideNeeds::from_needs(&needs)?;

    assert_eq!(
        read.review,
        ReviewJob {
            result: assay_app::JobResult::Success,
            stage: ReviewStage::NotRun,
            cost: Aeu::from_micros(0),
            documents: Vec::new(),
        }
    );
    assert_matches!(read.plan, PlanEnd::Succeeded(_));
    Ok(())
}
