//! `publish`'s persistence (S9, `docs/architecture/02-architecture.md` §5): the `RunInputs`
//! bundle and its schema, the evidence pointer and record (R3-08, R3-17), and what a persist
//! failure does: `failure persist-error` on a trust-path or exec-surface PR, a warning otherwise.

use std::assert_matches;
use std::error::Error;
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_app::{
    ArtifactJob, BUNDLE_FILE, BundleError, CheckConclusion, CheckState, Contract,
    ContractValidator, LlmEvidence, PersistFailure, ReviewState, Sha256Digest, VerifierTier,
    next_carry, verify,
};
use assay_domain::{
    Deferral, Event, EventPayload, EventSchema, FileChange, Lines, LlmStage, NotRunReason,
    Override, OverrideScope, PrClass, Readiness, Severity, decide,
};
use serde_json::{Value, json};

use crate::decide_fakes::Scenario;
use crate::eligibility_fakes::{APPROVER, By, CONTRIBUTOR, FIRST, World};
use crate::plan_fakes::{FakeError, FakePr, run_uid};
use crate::publish_fakes::{Call, FakeWorld};
use crate::publish_support::{HEAD_COMMIT, NAME, Run, STAMP, stamped, written};
use crate::verify_fakes::{REPOSITORY, Scripted, bundle as prompt_bundle, finding, request};

/// The one upload's artifact, file and bytes.
fn upload(world: &FakeWorld) -> Result<(String, String, Vec<u8>), Box<dyn Error>> {
    let uploads = world.uploads.borrow();
    let [upload] = uploads.as_slice() else {
        return Err(format!("expected one upload, got {}", uploads.len()).into());
    };
    Ok(upload.clone())
}

/// The uploaded bundle, parsed.
fn bundle(world: &FakeWorld) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(&upload(world)?.2)?)
}

/// Schema violations as (instance path, keyword).
type Violations = Vec<(String, String)>;

/// One LLM-stage case: its name, how it changes the run, and the record's expected state.
type LlmCase = (&'static str, fn(&mut Run), LlmEvidence);

/// Where `document` breaks `schemas/run-inputs-1.schema.json`: each violation's instance path
/// and keyword, in the order the validator reports them.
fn violations(document: &Value) -> Result<Violations, Box<dyn Error>> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/run-inputs-1.schema.json");
    let schema: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let validator = jsonschema::validator_for(&schema)?;
    Ok(validator
        .iter_errors(document)
        .map(|error| {
            (
                error.instance_path().as_str().to_owned(),
                error.kind().keyword().to_owned(),
            )
        })
        .collect())
}

/// Each violation list of the `run-inputs` fixtures of `validity` (`valid` or `invalid`), by
/// file stem in name order.
fn fixture_violations(validity: &str) -> Result<Vec<(String, Violations)>, Box<dyn Error>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/fixtures/run-inputs")
        .join(validity);
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("a fixture name")?;
        let document: Value = serde_json::from_slice(&fs::read(&path)?)?;
        found.push((stem.to_owned(), violations(&document)?));
    }
    found.sort();
    Ok(found)
}

/// Each invalid run-inputs fixture, with the one instance path and keyword it violates.
const REJECTED: [(&str, &str, &str); 18] = [
    (
        "carried-unknown-field",
        "/carry/findings/0",
        "additionalProperties",
    ),
    ("carried-uppercase-fp", "/carry/findings/0/fp", "pattern"),
    (
        "github-counted-approval-with-reason",
        "/github/approvals/0",
        "not",
    ),
    (
        "github-ineligible-approval-without-reason",
        "/github/approvals/0",
        "required",
    ),
    ("github-push-clock-not-utc", "/github/push_clock", "oneOf"),
    (
        "github-review-id-zero",
        "/github/approvals/0/review",
        "minimum",
    ),
    (
        "github-unknown-approval-status",
        "/github/approvals/0/status",
        "enum",
    ),
    (
        "github-valid-command-with-reason",
        "/github/commands/0",
        "not",
    ),
    (
        "github-void-command-with-ineligible-reason",
        "/github/commands/0/reason",
        "enum",
    ),
    (
        "github-void-command-without-reason",
        "/github/commands/0",
        "required",
    ),
    ("missing-carry", "", "required"),
    ("missing-events", "", "required"),
    ("missing-github", "", "required"),
    ("omitted-unknown-reason", "/carry/omitted/0/reason", "enum"),
    ("unknown-field", "", "additionalProperties"),
    ("unknown-job", "/documents/0/job", "enum"),
    ("unknown-version", "/schema", "const"),
    ("uppercase-digest", "/documents/0/sha256", "pattern"),
];

#[test]
fn the_run_inputs_fixtures_pass_or_fail_their_schema_where_listed() -> Result<(), Box<dyn Error>> {
    let accepted = fixture_violations("valid")?;
    let rejected = fixture_violations("invalid")?;

    assert_eq!(
        accepted,
        [
            ("carried".to_owned(), Vec::new()),
            ("decided".to_owned(), Vec::new())
        ]
    );
    let expected: Vec<(String, Vec<(String, String)>)> = REJECTED
        .iter()
        .map(|&(name, path, keyword)| {
            (name.to_owned(), vec![(path.to_owned(), keyword.to_owned())])
        })
        .collect();
    assert_eq!(rejected, expected);
    Ok(())
}

/// A PR changing a CI workflow (a trust path).
fn trust_path_pr() -> FakePr {
    let mut pr = FakePr::new();
    pr.changes = assay_app::ChangedFiles::Listed(vec![FileChange::new(
        ".github/workflows/ci.yml",
        Lines::Counted {
            added: 1,
            removed: 0,
        },
    )]);
    pr
}

/// A run of a PR changing a CI workflow (a trust path), overridden with `all` so the rubric
/// alone would publish `neutral`.
fn trust_path_run() -> Result<Run, Box<dyn Error>> {
    overridden(&Scenario::of(trust_path_pr())?)
}

/// A run whose head has no package set, so it is exec surface (and only that), overridden
/// with `all` so the rubric alone would publish `neutral`.
fn exec_surface_run() -> Result<Run, Box<dyn Error>> {
    let scenario = Scenario::requested(FakePr::new(), |request| request.head_packages = None)?;
    let run = overridden(&scenario)?;
    assert_eq!(
        run.authenticated.tiering().classes,
        [PrClass::ExecSurface],
        "the run is exec surface only"
    );
    Ok(run)
}

/// `scenario`'s run with an `all` override of its current reasons.
fn overridden(scenario: &Scenario) -> Result<Run, Box<dyn Error>> {
    let mut run = Run::of(scenario)?;
    let reasons_digest = decide(&run.rubric, &run.inputs).reasons_digest();
    run.inputs.override_record = Some(Override {
        scope: OverrideScope::All,
        reasons_digest,
        by: "owner".to_owned(),
    });
    Ok(run)
}

/// One `LedgerWritten` event of `job` at `idx`.
fn event(job: &str, idx: u64) -> Result<Event, Box<dyn Error>> {
    Ok(Event {
        schema: EventSchema::V1,
        job: job.to_owned(),
        idx,
        ts: "2026-10-12T00:00:00Z".to_owned(),
        run_key: "run-42-1".to_owned(),
        run_uid: run_uid(1)?,
        stage: "decide".to_owned(),
        payload: EventPayload::LedgerWritten {},
    })
}

// --- the bundle and its pointer ---

#[test]
fn republishing_a_run_uploads_the_same_bundle_and_still_persists() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    let first = written(run.publish(&world))?;
    world.restamp(STAMP);

    let second = written(run.publish(&world))?;

    assert_matches!((first.persist, second.persist), (None, None));
    let uploads = world
        .calls
        .borrow()
        .iter()
        .filter(|call| **call == Call::Upload)
        .count();
    assert_eq!(uploads, 2, "the bundle is uploaded again");
    let pointers: Vec<Sha256Digest> = world
        .evidence
        .borrow()
        .iter()
        .map(|entry| entry.pointer.sha256.clone())
        .collect();
    let digest = Sha256Digest::of(&upload(&world)?.2);
    assert_eq!(pointers, [digest.clone(), digest]);
    Ok(())
}

#[test]
fn the_pointer_s_digest_is_the_uploaded_bundle_s() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    let (artifact, file, bytes) = upload(&world)?;
    let evidence = world.evidence.borrow();
    let [entry] = evidence.as_slice() else {
        return Err("one evidence entry".into());
    };
    let digest = Sha256Digest::of(&bytes);
    assert_eq!(
        (Some(&artifact), file.as_str()),
        (
            run.authenticated.artifact_names().get(&ArtifactJob::Decide),
            BUNDLE_FILE
        )
    );
    assert_eq!(
        (&entry.pointer.sha256, &entry.record.bundle_sha256),
        (&digest, &digest),
        "R3-08"
    );
    assert_eq!(
        (
            entry.pointer.run_id.get(),
            entry.pointer.run_attempt.get(),
            entry.pr.get()
        ),
        (7, 1, 42)
    );
    assert_eq!(entry.repo_id, run.request.plan.run_uid.repo_id);
    assert_eq!(
        entry.record.evidence_key,
        run.authenticated.run_key().to_string()
    );
    assert_eq!(
        (entry.record.llm, entry.record.tier),
        (LlmEvidence::Complete, run.authenticated.tier())
    );
    let text = world.app_check(NAME)?.output.ok_or("written")?.text;
    assert!(
        text.contains(digest.as_str()),
        "the text projects the pointer: {text}"
    );
    Ok(())
}

/// #58: the bundle records the GitHub state snapshot (lists, range, pushers, push clock,
/// approvals) within its schema.
#[test]
fn the_bundle_records_the_github_state_snapshot_within_its_schema() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    let mut pr = World::opened_by(CONTRIBUTOR);
    pr.commit(HEAD_COMMIT, &[FIRST], By::user(CONTRIBUTOR));
    pr.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 4));
    pr.push((FIRST, HEAD_COMMIT), CONTRIBUTOR, 5);
    pr.head(HEAD_COMMIT);
    run.github = pr.state()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    let bundle = bundle(&world)?;
    assert_eq!(violations(&bundle)?, Vec::<(String, String)>::new());
    let github = &bundle["github"];
    assert_eq!(
        github["lists"]["maintainers"],
        json!([{ "id": 124_330_161, "login": "Ryan-Britton" }])
    );
    assert_eq!(
        github["range"],
        json!({ "status": "within", "users": [CONTRIBUTOR], "unattributed": false })
    );
    assert_eq!(github["pushers"], json!([CONTRIBUTOR]));
    assert_eq!(github["push_clock"], json!("2026-10-13T09:05:00Z"));
    assert_eq!(
        github["approvals"],
        json!([{
            "review": 1,
            "reviewer": APPROVER,
            "submitted_at": "2026-10-13T09:04:00Z",
            "status": "before-push"
        }])
    );
    assert_eq!(github["commands"], json!([]));
    Ok(())
}

/// #224: the bundle records the carry set, and a bundle carrying findings still satisfies the
/// schema.
#[test]
fn the_bundle_records_the_carry_set_within_its_schema() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    let mut second = finding(1, Severity::Minor)?;
    second.fingerprint = "00000000000000aa".parse()?;
    second.finding.end_line = NonZeroU64::new(13);
    let raised = [finding(0, Severity::Major)?, second];
    let prompts = prompt_bundle()?;
    let verification = verify(
        &Scripted::new([]),
        request(VerifierTier::Micro, &raised, &prompts),
    );
    run.carry = next_carry(Vec::new(), &raised, &verification, REPOSITORY);
    let world = stamped()?;

    written(run.publish(&world))?;

    let bundle = bundle(&world)?;
    assert_eq!(violations(&bundle)?, Vec::<(String, String)>::new());
    assert_eq!(
        bundle["carry"],
        json!({
            "findings": [
                {
                    "fp": "0123456789abcdef",
                    "file": "src/lib.rs",
                    "line": 12,
                    "class": "error-handling",
                    "severity": "major",
                    "title": "unwrap on user input panics (0)",
                    "trigger": "a non-numeric input reaches line 12 through parse",
                    "evidence": "let x = input.parse().unwrap();"
                },
                {
                    "fp": "00000000000000aa",
                    "file": "src/lib.rs",
                    "line": 12,
                    "end_line": 13,
                    "class": "error-handling",
                    "severity": "minor",
                    "title": "unwrap on user input panics (1)",
                    "trigger": "a non-numeric input reaches line 12 through parse",
                    "evidence": "let x = input.parse().unwrap();"
                }
            ],
            "omitted": []
        })
    );
    Ok(())
}

#[test]
fn the_bundle_satisfies_its_schema_and_holds_each_document_s_exact_text()
-> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_review(
        "combined-micro.json",
        br#"{"not": "validated here"}"#.to_vec(),
    )?;
    let mut run = Run::of(&scenario)?;
    run.events = vec![event("decide", 0)?];
    let world = stamped()?;

    written(run.publish(&world))?;

    let bundle = bundle(&world)?;
    assert_eq!(violations(&bundle)?, Vec::<(String, String)>::new());
    let decision = serde_json::to_vec(&bundle["decision"])?;
    ContractValidator::new()?.read(Contract::Decision, &decision)?;
    let documents: Vec<(&str, &str, &str)> = bundle["documents"]
        .as_array()
        .ok_or("documents")?
        .iter()
        .map(|d| {
            (
                d["job"].as_str().unwrap_or_default(),
                d["file"].as_str().unwrap_or_default(),
                d["text"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    let plan = String::from_utf8(scenario.plan_json.clone())?;
    let jobs: Vec<(&str, &str)> = documents
        .iter()
        .map(|(job, file, _)| (*job, *file))
        .collect();
    assert_eq!(
        jobs,
        [
            ("plan", "run.json"),
            ("gates-static", "temper.json"),
            ("gates-lint", "temper.json"),
            ("gates-test", "custom-lint.json"),
            ("review", "combined-micro.json"),
        ]
    );
    assert_eq!(documents.first().map(|d| d.2), Some(plan.as_str()));
    assert_eq!(
        documents.last().map(|d| d.2),
        Some(r#"{"not": "validated here"}"#)
    );
    assert_eq!(bundle["inputs"], serde_json::to_value(&run.inputs)?);
    Ok(())
}

#[test]
fn the_bundle_merges_events_in_job_then_index_order() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    run.events = vec![event("plan", 2)?, event("decide", 0)?, event("plan", 1)?];
    let world = stamped()?;

    written(run.publish(&world))?;

    let bundle = bundle(&world)?;
    let order: Vec<(String, u64)> = bundle["events"]
        .as_array()
        .ok_or("events")?
        .iter()
        .map(|e| {
            (
                e["job"].as_str().unwrap_or_default().to_owned(),
                e["idx"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("decide".to_owned(), 0),
            ("plan".to_owned(), 1),
            ("plan".to_owned(), 2)
        ]
    );
    Ok(())
}

// --- persist failures ---

#[test]
fn a_failed_upload_fails_a_trust_path_pr_closed() -> Result<(), Box<dyn Error>> {
    let run = trust_path_run()?;
    let world = stamped()?;
    world.fail(Call::Upload, 1);

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    assert_matches!(written.persist, Some(PersistFailure::Upload(FakeError)));
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("persist-error: upload"), "{summary}");
    assert_eq!(*world.evidence.borrow(), [], "no pointer without a bundle");
    Ok(())
}

#[test]
fn a_trust_path_pr_that_persists_keeps_its_override() -> Result<(), Box<dyn Error>> {
    let run = trust_path_run()?;
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Neutral)
    );
    Ok(())
}

#[test]
fn a_failed_upload_only_warns_on_another_pr() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.fail(Call::Upload, 1);

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Success)
    );
    assert_matches!(written.persist, Some(PersistFailure::Upload(FakeError)));
    let output = world.app_check(NAME)?.output.ok_or("written")?;
    assert!(
        output.summary.contains("**Warning:**"),
        "{}",
        output.summary
    );
    assert!(output.summary.contains("(`upload`)"), "{}", output.summary);
    assert!(
        output.text.contains("evidence pointer: none"),
        "{}",
        output.text
    );
    assert_eq!(*world.evidence.borrow(), []);
    Ok(())
}

#[test]
fn a_failed_ledger_write_fails_a_trust_path_pr_closed() -> Result<(), Box<dyn Error>> {
    let run = trust_path_run()?;
    let world = stamped()?;
    world.fail(Call::RecordEvidence, 1);

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    assert_matches!(written.persist, Some(PersistFailure::Ledger(FakeError)));
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("persist-error: ledger"), "{summary}");
    Ok(())
}

#[test]
fn a_failed_ledger_write_only_warns_on_another_pr() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.fail(Call::RecordEvidence, 1);

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Success)
    );
    assert_matches!(written.persist, Some(PersistFailure::Ledger(FakeError)));
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("(`ledger`)"), "{summary}");
    Ok(())
}

#[test]
fn a_review_document_that_is_not_utf8_is_not_persisted() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_review("combined-micro.json", vec![b'{', 0xff, b'}'])?;
    let run = Run::of(&scenario)?;
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    assert_matches!(
        written.persist,
        Some(PersistFailure::Bundle(BundleError::NotUtf8 { job: ArtifactJob::Review, ref file }))
            if file == "combined-micro.json"
    );
    assert_eq!(*world.uploads.borrow(), []);
    Ok(())
}

#[test]
fn a_bundle_over_its_cap_is_not_persisted() -> Result<(), Box<dyn Error>> {
    // JSON writes each 0x01 as `\u0001`, six bytes: three 8 MiB documents need 144 MiB.
    let mut scenario = Scenario::new()?;
    for name in ["a", "b", "c"] {
        scenario.publish_review(
            &format!("{name}.json"),
            vec![1; assay_app::MAX_DOCUMENT_BYTES],
        )?;
    }
    let run = Run::of(&scenario)?;
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    assert_matches!(
        written.persist,
        Some(PersistFailure::Bundle(BundleError::TooLarge))
    );
    assert_eq!(*world.uploads.borrow(), []);
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("(`bundle-too-large`)"), "{summary}");
    Ok(())
}

#[test]
fn a_trust_path_pr_whose_bundle_is_over_its_cap_fails_closed() -> Result<(), Box<dyn Error>> {
    // As in `a_bundle_over_its_cap_is_not_persisted`: 144 MiB of JSON, past the 96 MiB cap.
    let mut scenario = Scenario::of(trust_path_pr())?;
    for name in ["a", "b", "c"] {
        scenario.publish_review(
            &format!("{name}.json"),
            vec![1; assay_app::MAX_DOCUMENT_BYTES],
        )?;
    }
    let run = overridden(&scenario)?;
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(
        summary.contains("persist-error: bundle-too-large"),
        "{summary}"
    );
    assert_eq!(*world.uploads.borrow(), []);
    Ok(())
}

#[test]
fn a_trust_path_pr_whose_bundle_is_not_utf8_fails_closed() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::of(trust_path_pr())?;
    scenario.publish_review("combined-micro.json", vec![b'{', 0xff, b'}'])?;
    let run = overridden(&scenario)?;
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(
        summary.contains("persist-error: bundle-not-utf8"),
        "{summary}"
    );
    assert_eq!(*world.uploads.borrow(), []);
    assert_eq!(*world.evidence.borrow(), [], "no pointer without a bundle");
    Ok(())
}

#[test]
fn a_failed_upload_fails_an_exec_surface_pr_closed() -> Result<(), Box<dyn Error>> {
    let run = exec_surface_run()?;
    let world = stamped()?;
    world.fail(Call::Upload, 1);

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("persist-error: upload"), "{summary}");
    Ok(())
}

#[test]
fn an_exec_surface_pr_that_persists_keeps_its_override() -> Result<(), Box<dyn Error>> {
    let run = exec_surface_run()?;
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Neutral)
    );
    Ok(())
}

// --- the evidence record's LLM state (R3-17) ---

#[test]
fn the_record_says_how_the_llm_stage_ran() -> Result<(), Box<dyn Error>> {
    let cases: [LlmCase; 5] = [
        ("completed", |_| {}, LlmEvidence::Complete),
        (
            "a required role missing",
            |run| run.inputs.completed_roles.clear(),
            LlmEvidence::Degraded,
        ),
        (
            "a real draft",
            |run| {
                run.inputs.pr.readiness = Readiness::Draft;
                run.inputs.llm = LlmStage::NotRun {
                    reason: NotRunReason::Draft,
                };
            },
            LlmEvidence::SkippedDraft,
        ),
        (
            "deferred",
            |run| {
                run.inputs.llm = LlmStage::Deferred {
                    reason: Deferral::SlotBusy,
                };
            },
            LlmEvidence::Deferred,
        ),
        (
            "not run on a reviewed tier",
            |run| {
                run.inputs.llm = LlmStage::NotRun {
                    reason: NotRunReason::NoLlmTier,
                };
            },
            LlmEvidence::Deferred,
        ),
    ];
    for (case, arrange, expected) in cases {
        let mut run = Run::new()?;
        arrange(&mut run);
        let world = stamped()?;

        written(run.publish(&world))?;

        let llm = world
            .evidence
            .borrow()
            .first()
            .map(|entry| entry.record.llm);
        assert_eq!(llm, Some(expected), "{case}");
    }
    Ok(())
}
