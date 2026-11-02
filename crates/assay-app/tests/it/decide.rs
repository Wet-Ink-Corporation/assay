//! `authenticate` (Z4, `docs/architecture/02-architecture.md` §1 and §4): what `decide` believes
//! only after checking it, against fakes of its ports and artifacts tampered in transit.

use std::assert_matches;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    ArtifactJob, AuthenticatedRun, Authentication, CommitRange, DecideError, GateError, JobResult,
    Moved, OutputsProblem, PlanMismatch, PullOnHead, Refusal, Reported, ReportedGate, Snapshot,
    TierDegrade, Unreadable, UpstreamJob,
};
use assay_domain::{
    Authorship, Conclusion as Verdict, DecisionInputs, FileChange, GateConclusion, GateOutcome,
    InfraPolicy, Lines, LlmStage, OwnerApproval, PrClass, Readiness, Reason, ReasonCode,
    RequiredRole, StepAttribution, SupportOutcome, Tier, WeighIn, decide,
};
use serde_json::json;

use crate::decide_fakes::{Scenario, authenticated, gate_document, gate_finding, refusal};
use crate::plan_fakes::{FakePr, HEAD, run_uid};
use crate::support::rubric_v1;

/// A ready PR by someone other than an agent, not approved, never requeued.
const SNAPSHOT: Snapshot = Snapshot {
    owner_approval: OwnerApproval::NotApproved,
    authorship: Authorship::Other,
    readiness: Readiness::Ready,
    requeues: 0,
    commits: CommitRange::Within,
};

/// The decision inputs of `run`: its authenticated facts, with a completed LLM stage in which
/// exactly `completed` reported.
pub(crate) fn inputs(run: &AuthenticatedRun, completed: &[RequiredRole]) -> DecisionInputs {
    DecisionInputs {
        decision_key: "dec".to_owned(),
        evidence_key: run.run_key().to_string(),
        policy: run.policy().clone(),
        pr: run.pull_request(SNAPSHOT),
        gates: run.gates().to_vec(),
        weigh_in: WeighIn::NotRequired,
        llm: LlmStage::Completed,
        findings: Vec::new(),
        dispositions: Vec::new(),
        completed_roles: completed.iter().copied().collect(),
        coordinator: SupportOutcome::Completed,
        verifier: SupportOutcome::NotNeeded,
        degraded: Vec::new(),
        waivers: Vec::new(),
        override_record: None,
    }
}

/// A PR whose one changed file has `lines` added lines: over 150 tiers it `full` without a
/// class.
fn pr_with_lines(lines: u64) -> FakePr {
    let mut pr = FakePr::new();
    pr.changes = assay_app::ChangedFiles::Listed(vec![FileChange::new(
        "src/lib.rs",
        Lines::Counted {
            added: lines,
            removed: 0,
        },
    )]);
    pr
}

// --- before anything is read ---

#[test]
fn a_rerun_is_refused_before_any_port_is_called() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.plan.run_uid = run_uid(2)?;
    scenario.state.failing = true;
    scenario.artifacts.failing = true;

    let outcome = scenario.authenticate()?;

    assert_matches!(
        outcome,
        Err(DecideError::NotFirstAttempt { attempt }) if attempt.get() == 2
    );
    Ok(())
}

#[test]
fn a_moved_head_supersedes_the_run_before_any_artifact_is_read() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.state.current.head_sha = "cccccccccccccccccccccccccccccccccccccccc".parse()?;
    scenario.artifacts.failing = true;

    let outcome = scenario.authenticate()?;

    assert_matches!(outcome, Ok(Authentication::Superseded(Moved::Head)));
    Ok(())
}

#[test]
fn a_retargeted_base_supersedes_the_run_and_writes_no_check() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    "release".clone_into(&mut scenario.state.current.base_ref);
    scenario.artifacts.failing = true;

    let outcome = scenario.authenticate()?;

    assert_matches!(outcome, Ok(Authentication::Superseded(Moved::BaseRef)));
    Ok(())
}

#[test]
fn a_cancelled_upstream_with_a_newer_run_publishes_nothing() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.review.result = JobResult::Cancelled;
    scenario.state.newer_run = true;
    scenario.artifacts.failing = true;

    let outcome = scenario.authenticate()?;

    assert_matches!(outcome, Ok(Authentication::YieldToNewer));
    Ok(())
}

#[test]
fn a_cancel_without_a_newer_run_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_test.result = JobResult::Cancelled;
    scenario.artifacts.failing = true;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Cancelled {
            job: UpstreamJob::GatesTest
        }
    );
    assert_eq!(
        refused.reason(),
        Reason {
            code: ReasonCode::GateError,
            detail: "gates-test: cancelled".to_owned(),
            fp: None,
        }
    );
    Ok(())
}

// --- an honest run ---

#[test]
fn an_honest_run_is_authenticated_with_its_recomputed_tier_and_gates() -> Result<(), Box<dyn Error>>
{
    let scenario = Scenario::new()?;

    let run = authenticated(scenario.authenticate()?)?;

    let gates: Vec<(&str, bool, &GateOutcome)> = run
        .gates()
        .iter()
        .map(|gate| (gate.name.as_str(), gate.required, &gate.outcome))
        .collect();
    let passed = GateOutcome::Ran {
        conclusion: GateConclusion::Pass,
        steps: StepAttribution::Unknown,
    };
    assert_eq!(
        (run.tier(), run.open_prs_on_head(), gates),
        (
            Tier::Micro,
            1,
            vec![
                ("temper", true, &passed),
                ("temper", true, &passed),
                ("custom-lint", false, &passed),
            ]
        )
    );
    Ok(())
}

// --- tampered artifacts ---

#[test]
fn a_tampered_plan_artifact_is_refused_by_its_digest() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let artifact = scenario.request.plan_job.artifact.clone();
    let mut forged = scenario.plan_value()?;
    forged["tier"] = json!("trivial");
    scenario
        .artifacts
        .put(&artifact, "run.json", serde_json::to_vec(&forged)?);

    let refused = refusal(scenario.authenticate()?)?;

    assert_eq!(
        refused.reason(),
        Reason {
            code: ReasonCode::GateError,
            detail: "artifact-digest: plan/run.json".to_owned(),
            fp: None,
        }
    );
    Ok(())
}

#[test]
fn a_deleted_plan_artifact_is_refused_as_missing() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let artifact = scenario.request.plan_job.artifact.clone();
    scenario.artifacts.remove(&artifact, "run.json");

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::ArtifactUnreadable {
            job: ArtifactJob::Plan,
            ref file,
            why: Unreadable::Missing,
        } if file == "run.json"
    );
    Ok(())
}

#[test]
fn a_tampered_gate_document_is_refused_even_for_an_optional_gate() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let artifact = scenario.artifact(ArtifactJob::GatesTest)?;
    scenario.artifacts.put(
        &artifact,
        "custom-lint.json",
        gate_document("custom-lint", "fail", &json!([])),
    );

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::ArtifactDigest {
            job: ArtifactJob::GatesTest,
            ref file,
        } if file == "custom-lint.json"
    );
    Ok(())
}

#[test]
fn a_tampered_review_document_is_refused_by_its_digest() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let artifact = scenario.artifact(ArtifactJob::Review)?;
    scenario
        .request
        .review
        .documents
        .push(assay_app::ArtifactDocument {
            file: "combined.json".to_owned(),
            digest: assay_app::Sha256Digest::of(b"what the reviewer wrote"),
        });
    scenario
        .artifacts
        .put(&artifact, "combined.json", b"what reached decide".to_vec());

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::ArtifactDigest {
            job: ArtifactJob::Review,
            ref file,
        } if file == "combined.json"
    );
    Ok(())
}

#[test]
fn an_authentic_review_document_is_handed_on_unread() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let artifact = scenario.artifact(ArtifactJob::Review)?;
    let bytes = b"{\"schema\": \"assay.findings/1\"}".to_vec();
    scenario
        .request
        .review
        .documents
        .push(assay_app::ArtifactDocument {
            file: "combined.json".to_owned(),
            digest: assay_app::Sha256Digest::of(&bytes),
        });
    scenario
        .artifacts
        .put(&artifact, "combined.json", bytes.clone());

    let run = authenticated(scenario.authenticate()?)?;

    assert_eq!(run.review_documents(), [bytes]);
    Ok(())
}

// --- the plan, recomputed ---

#[test]
fn a_plan_that_breaks_its_schema_is_a_plan_mismatch() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let mut forged = scenario.plan_value()?;
    forged["unexpected"] = json!(true);
    scenario.publish_plan(serde_json::to_vec(&forged)?);

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(refused, Refusal::PlanMismatch(PlanMismatch::Invalid));
    Ok(())
}

#[test]
fn a_plan_that_claims_a_lower_tier_is_a_plan_mismatch_naming_the_tier() -> Result<(), Box<dyn Error>>
{
    let mut scenario = Scenario::new()?;
    let mut forged = scenario.plan_value()?;
    forged["tier"] = json!("trivial");
    scenario.publish_plan(serde_json::to_vec(&forged)?);

    let refused = refusal(scenario.authenticate()?)?;

    assert_eq!(
        refused.reason(),
        Reason {
            code: ReasonCode::PlanMismatch,
            detail: "tier".to_owned(),
            fp: None,
        }
    );
    Ok(())
}

#[test]
fn a_plan_that_drops_a_required_gate_is_a_plan_mismatch() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let mut forged = scenario.plan_value()?;
    forged["plugins"] = json!([]);
    scenario.publish_plan(serde_json::to_vec(&forged)?);

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::PlanMismatch(PlanMismatch::Field { ref name }) if name == "plugins"
    );
    Ok(())
}

#[test]
fn a_plan_whose_artifact_name_is_not_a_nonce_name_is_a_plan_mismatch() -> Result<(), Box<dyn Error>>
{
    let mut scenario = Scenario::new()?;
    let mut forged = scenario.plan_value()?;
    forged["artifact_names"]["review"] = json!("assay-review-guessable");
    scenario.publish_plan(serde_json::to_vec(&forged)?);

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(refused, Refusal::PlanMismatch(PlanMismatch::ArtifactNames));
    Ok(())
}

#[test]
fn a_plan_naming_another_plan_artifact_than_its_job_is_a_plan_mismatch()
-> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let mut forged = scenario.plan_value()?;
    forged["artifact_names"]["plan"] = json!("assay-plan-ffffffffffffffffffffffffffffffff");
    scenario.publish_plan(serde_json::to_vec(&forged)?);

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(refused, Refusal::PlanMismatch(PlanMismatch::ArtifactNames));
    Ok(())
}

#[test]
fn a_base_config_z4_cannot_read_is_a_plan_mismatch() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario
        .pr
        .remove(crate::plan_fakes::BASE, ".assay/plugins.toml");

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(refused, Refusal::PlanMismatch(PlanMismatch::Recompute(_)));
    Ok(())
}

#[test]
fn a_plan_job_that_failed_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.plan_job.result = JobResult::Failure;

    let refused = refusal(scenario.authenticate()?)?;

    assert_eq!(
        refused.reason(),
        Reason {
            code: ReasonCode::GateError,
            detail: "plan: failure".to_owned(),
            fp: None,
        }
    );
    Ok(())
}

// --- admit's degrade (R3-27) ---

#[test]
fn admits_full_to_lite_degrade_is_not_reported_as_a_missing_role() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::of(pr_with_lines(400))?;
    scenario.request.admit.degrade = TierDegrade::FullToLite;

    let run = authenticated(scenario.authenticate()?)?;
    let decision = decide(&rubric_v1()?, &inputs(&run, &[RequiredRole::Combined]));

    assert_eq!(
        (run.tier(), decision.conclusion, decision.degraded),
        (Tier::Lite, Verdict::Success, Vec::new())
    );
    Ok(())
}

#[test]
fn without_a_degrade_the_full_tier_requires_its_roles() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::of(pr_with_lines(400))?;

    let run = authenticated(scenario.authenticate()?)?;
    let decision = decide(&rubric_v1()?, &inputs(&run, &[RequiredRole::Combined]));

    let missing: Vec<&str> = decision.degraded.iter().map(|d| d.role.as_str()).collect();
    assert_eq!(
        (run.tier(), missing),
        (Tier::Full, vec!["bugs", "design", "security"])
    );
    Ok(())
}

#[test]
fn a_degrade_of_a_trust_path_run_is_a_plan_mismatch() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with_lines(400);
    pr.changes = assay_app::ChangedFiles::Listed(vec![FileChange::new(
        ".github/workflows/ci.yml",
        Lines::Counted {
            added: 1,
            removed: 0,
        },
    )]);
    let mut scenario = Scenario::of(pr)?;
    scenario.request.admit.degrade = TierDegrade::FullToLite;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::PlanMismatch(PlanMismatch::Degrade {
            tier: Tier::Full,
            ref classes,
        }) if *classes == [PrClass::TrustPath]
    );
    Ok(())
}

#[test]
fn a_degrade_of_a_run_that_is_not_full_is_a_plan_mismatch() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.admit.degrade = TierDegrade::FullToLite;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::PlanMismatch(PlanMismatch::Degrade {
            tier: Tier::Micro,
            ref classes,
        }) if classes.is_empty()
    );
    Ok(())
}

#[test]
fn a_degrade_from_an_admit_that_did_not_succeed_is_a_plan_mismatch() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::of(pr_with_lines(400))?;
    scenario.request.admit = assay_app::AdmitJob {
        result: JobResult::Failure,
        degrade: TierDegrade::FullToLite,
        ..scenario.request.admit
    };

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::PlanMismatch(PlanMismatch::Degrade {
            tier: Tier::Full,
            ref classes,
        }) if classes.is_empty()
    );
    Ok(())
}

// --- one PR per head (R3-19) ---

#[test]
fn a_second_open_pr_on_the_head_into_the_base_fails_ambiguous_head() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.state.pulls.push(PullOnHead {
        number: NonZeroU64::new(43).ok_or("not zero")?,
        open: true,
        base_ref: "main".to_owned(),
    });

    let run = authenticated(scenario.authenticate()?)?;
    let decision = decide(&rubric_v1()?, &inputs(&run, &[RequiredRole::CombinedMicro]));

    let codes: Vec<ReasonCode> = decision.reasons.iter().map(|r| r.code).collect();
    assert_eq!(
        (run.open_prs_on_head(), decision.conclusion, codes),
        (2, Verdict::Failure, vec![ReasonCode::AmbiguousHead])
    );
    Ok(())
}

#[test]
fn closed_prs_and_prs_into_other_bases_do_not_count() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.state.pulls = vec![
        PullOnHead {
            number: NonZeroU64::new(41).ok_or("not zero")?,
            open: false,
            base_ref: "main".to_owned(),
        },
        PullOnHead {
            number: NonZeroU64::new(44).ok_or("not zero")?,
            open: true,
            base_ref: "release".to_owned(),
        },
    ];

    let run = authenticated(scenario.authenticate()?)?;

    // The PR itself counts even when the listing lags behind.
    assert_eq!(run.open_prs_on_head(), 1);
    Ok(())
}

// --- gates ---

#[test]
fn a_skipped_gates_job_skips_its_required_gate() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_lint.result = JobResult::Skipped;
    scenario.request.gates_lint.gates.clear();

    let run = authenticated(scenario.authenticate()?)?;
    let decision = decide(&rubric_v1()?, &inputs(&run, &[RequiredRole::CombinedMicro]));

    assert_eq!(
        decision.reasons,
        vec![Reason {
            code: ReasonCode::GateSkipped,
            detail: "temper".to_owned(),
            fp: None,
        }]
    );
    Ok(())
}

#[test]
fn a_failed_gates_job_without_a_report_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_lint.result = JobResult::Failure;
    scenario.request.gates_lint.gates.clear();

    let run = authenticated(scenario.authenticate()?)?;

    let lint = run.gates().get(1).ok_or("the lint gate")?;
    assert_eq!(
        lint.outcome,
        GateOutcome::Error {
            detail: "job-failed".to_owned()
        }
    );
    Ok(())
}

#[test]
fn a_timed_out_gate_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_lint.gates = vec![ReportedGate {
        plugin: "temper".to_owned(),
        reported: Reported::Error(GateError::Timeout),
    }];

    let run = authenticated(scenario.authenticate()?)?;
    let decision = decide(&rubric_v1()?, &inputs(&run, &[RequiredRole::CombinedMicro]));

    assert_eq!(
        decision.reasons,
        vec![Reason {
            code: ReasonCode::GateError,
            detail: "temper: timeout".to_owned(),
            fp: None,
        }]
    );
    Ok(())
}

#[test]
fn a_gate_document_from_another_gate_is_an_invalid_document() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Pass,
        gate_document("custom-lint", "pass", &json!([])),
    )?;

    let run = authenticated(scenario.authenticate()?)?;

    let static_temper = run.gates().first().ok_or("the static gate")?;
    assert_eq!(
        static_temper.outcome,
        GateOutcome::Error {
            detail: "invalid-document".to_owned()
        }
    );
    Ok(())
}

#[test]
fn a_failing_exit_outweighs_a_passing_document() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Fail,
        gate_document("temper", "pass", &json!([])),
    )?;

    let run = authenticated(scenario.authenticate()?)?;

    let static_temper = run.gates().first().ok_or("the static gate")?;
    assert_eq!(
        static_temper.outcome,
        GateOutcome::Ran {
            conclusion: GateConclusion::Fail,
            steps: StepAttribution::Unknown,
        }
    );
    Ok(())
}

#[test]
fn a_report_for_a_plugin_the_job_does_not_run_is_refused() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_static.gates.push(ReportedGate {
        plugin: "custom-lint".to_owned(),
        reported: Reported::Error(GateError::NotRun),
    });

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::GatesStatic,
            problem: OutputsProblem::UnknownPlugin,
        }
    );
    Ok(())
}

#[test]
fn a_gate_finding_is_fingerprinted_on_the_tree_line_it_cites() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.write(
        HEAD,
        "src/lib.rs",
        b"pub fn two() {\n    let x = y.unwrap();\n    let x = y.unwrap();\n}\n",
    );
    let mut scenario = Scenario::of(pr)?;
    let findings = json!([
        gate_finding(
            "clippy::unwrap_used",
            "src/lib.rs",
            2,
            "let x = y.unwrap();"
        ),
        gate_finding(
            "clippy::unwrap_used",
            "src/lib.rs",
            3,
            "let  x = y.unwrap();  // two"
        ),
    ]);
    scenario.publish_gate(
        (ArtifactJob::GatesLint, "temper"),
        GateConclusion::Fail,
        gate_document("temper", "fail", &findings),
    )?;

    let run = authenticated(scenario.authenticate()?)?;

    let lint = run.gates().get(1).ok_or("the lint gate")?;
    let evidence: Vec<&str> = run
        .gate_findings()
        .iter()
        .map(|finding| finding.evidence.as_str())
        .collect();
    let rules: Vec<Option<&str>> = lint.findings.iter().map(|f| f.rule.as_deref()).collect();
    let first = lint.findings.first().ok_or("a first finding")?.fp;
    let second = lint.findings.get(1).ok_or("a second finding")?.fp;
    assert_eq!(
        (evidence, rules),
        (
            vec!["    let x = y.unwrap();", "    let x = y.unwrap();"],
            vec![Some("clippy::unwrap_used"), Some("clippy::unwrap_used")]
        )
    );
    assert_ne!(
        first, second,
        "two occurrences of one line keep two fingerprints"
    );
    Ok(())
}

#[test]
fn a_gate_finding_whose_evidence_is_not_on_its_line_cannot_be_waived() -> Result<(), Box<dyn Error>>
{
    let mut scenario = Scenario::new()?;
    let findings = json!([gate_finding(
        "raised",
        "src/lib.rs",
        1,
        "something the tree does not say"
    )]);
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Fail,
        gate_document("temper", "fail", &findings),
    )?;

    let run = authenticated(scenario.authenticate()?)?;

    let static_temper = run.gates().first().ok_or("the static gate")?;
    let rules: Vec<Option<&str>> = static_temper
        .findings
        .iter()
        .map(|f| f.rule.as_deref())
        .collect();
    assert_eq!(rules, vec![None]);
    Ok(())
}

#[test]
fn a_gate_finding_with_no_content_to_fingerprint_errors_its_gate() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let findings = json!([gate_finding("raised", "missing.rs", 1, "…")]);
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Fail,
        gate_document("temper", "fail", &findings),
    )?;

    let run = authenticated(scenario.authenticate()?)?;

    let static_temper = run.gates().first().ok_or("the static gate")?;
    assert_eq!(
        (&static_temper.outcome, static_temper.findings.len()),
        (
            &GateOutcome::Error {
                detail: "evidence-missing".to_owned()
            },
            0
        )
    );
    Ok(())
}

#[test]
fn the_policy_comes_from_base_config() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::new()?;

    let run = authenticated(scenario.authenticate()?)?;

    let policy = run.policy();
    assert_eq!(
        (
            policy.on_infra_failure,
            policy.block_on_infra_for.as_slice(),
            policy.protected_bases.as_slice()
        ),
        (
            InfraPolicy::Warn,
            [PrClass::TrustPath, PrClass::ExecSurface].as_slice(),
            ["main".to_owned()].as_slice()
        )
    );
    Ok(())
}

#[test]
fn a_failing_artifact_store_is_an_error_not_a_verdict() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.artifacts.failing = true;

    let outcome = scenario.authenticate()?;

    assert_matches!(
        outcome,
        Err(DecideError::Artifacts {
            job: ArtifactJob::Plan,
            ..
        })
    );
    Ok(())
}

#[test]
fn a_deleted_gate_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let artifact = scenario.artifact(ArtifactJob::GatesLint)?;
    scenario.artifacts.remove(&artifact, "temper.json");

    let run = authenticated(scenario.authenticate()?)?;

    let lint = run.gates().get(1).ok_or("the lint gate")?;
    assert_eq!(
        lint.outcome,
        GateOutcome::Error {
            detail: "artifact-missing".to_owned()
        }
    );
    Ok(())
}

#[test]
fn a_pull_request_carries_the_recomputed_facts_and_the_snapshot() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::new()?;

    let run = authenticated(scenario.authenticate()?)?;

    let pr = run.pull_request(SNAPSHOT);
    assert_eq!(
        (
            pr.base_ref.as_str(),
            pr.open_prs_on_head,
            pr.tier,
            pr.classes,
            pr.readiness
        ),
        ("main", 1, Tier::Micro, Vec::new(), Readiness::Ready)
    );
    Ok(())
}

#[test]
fn a_range_past_the_commit_cap_makes_the_pr_need_a_split() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::new()?;
    let run = authenticated(scenario.authenticate()?)?;

    let pr = run.pull_request(Snapshot {
        commits: CommitRange::NeedsSplit,
        ..SNAPSHOT
    });

    assert_eq!(pr.tier, Tier::NeedsSplit);
    Ok(())
}

#[test]
fn a_review_document_with_an_unsafe_file_name_is_refused() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario
        .request
        .review
        .documents
        .push(assay_app::ArtifactDocument {
            file: "../run.json".to_owned(),
            digest: assay_app::Sha256Digest::of(b""),
        });

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::Review,
            problem: OutputsProblem::FileName,
        }
    );
    Ok(())
}
