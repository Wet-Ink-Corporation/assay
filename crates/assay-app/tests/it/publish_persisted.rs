//! D5 A: `decide` prepares the bundle, the workflow uploads it, and `publish_persisted` re-reads
//! it, confirms the upload through `BundleSource` by sha256, then takes the lock and writes the
//! evidence pointer and the check (02 §5, S9, R3-08). Everything it publishes is rebuilt from the
//! bundle, so it publishes what `publish` publishes from memory.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    BUNDLE_FILE, BundleRefused, CheckConclusion, CheckState, CheckStatus, CheckUpdate, Decided,
    EvidenceEntry, MAX_BUNDLE_BYTES, PersistFailure, PersistOutcome, Persisted, PersistedError,
    PersistedOutcome, PersistedPorts, PrepareError, PreparedBundle, Published, ReviewState,
    RunField, Sha256Digest, Target, Unconfirmed, UnknownPersistOutcome, Written, prepare,
    publish_persisted,
};
use assay_domain::{FileChange, Lines, Override, OverrideScope, PrClass, Tier, decide};
use serde_json::Value;

use crate::decide_fakes::Scenario;
use crate::eligibility_fakes::{APPROVER, By, CONTRIBUTOR, FIRST, World};
use crate::plan_fakes::{FakeError, FakePr};
use crate::publish_fakes::{Call, FakeCheck, FakeWorld};
use crate::publish_support::{DEFAULT_BRANCH, HEAD_COMMIT, NAME, Run, stamped, written};
use crate::replay_fakes::{FakeBundles, Stored};

/// What `publish_persisted` returns over the fakes.
type Outcome = PersistedOutcome<FakeWorld, FakeWorld, FakeBundles>;

/// `run`'s decision as `decide` holds it.
fn decided(run: &Run) -> Decided<'_> {
    Decided {
        run: &run.authenticated,
        rubric: &run.rubric,
        inputs: &run.inputs,
        events: &run.events,
        carry: &run.carry,
        github: &run.github,
        cost: run.cost,
    }
}

/// `run`'s bundle, prepared.
fn prepared(run: &Run) -> Result<PreparedBundle, PrepareError> {
    prepare(&run.target(), &decided(run))
}

/// `bundle` of `run` to publish at `target`, the upload step having ended `upload`.
fn persisted<'a>(
    run: &'a Run,
    target: Target<'a>,
    (bundle, digest): (&'a [u8], &'a Sha256Digest),
    upload: PersistOutcome,
) -> Persisted<'a> {
    Persisted {
        target,
        rubric: &run.rubric,
        cost: run.cost,
        bundle,
        digest,
        upload,
    }
}

/// Publishes `persisted` over `world` and `bundles`.
fn publish_at(persisted: &Persisted<'_>, (world, bundles): (&FakeWorld, &FakeBundles)) -> Outcome {
    publish_persisted(
        persisted,
        &PersistedPorts {
            forge: world,
            ledger: world,
            bundles,
        },
    )
}

/// Publishes `run`'s prepared bundle, the upload step having ended `upload`.
fn publish_prepared(
    run: &Run,
    upload: PersistOutcome,
    (world, bundles): (&FakeWorld, &FakeBundles),
) -> Result<Outcome, Box<dyn Error>> {
    let bundle = prepared(run)?;
    let parts = (bundle.bytes(), bundle.digest());
    Ok(publish_at(
        &persisted(run, run.target(), parts, upload),
        (world, bundles),
    ))
}

/// What was written, or why nothing was.
fn persisted_written(
    outcome: Outcome,
) -> Result<Written<FakeError, Unconfirmed<FakeError>>, Box<dyn Error>> {
    match outcome? {
        Published::Written(written) => Ok(written),
        other => Err(format!("expected a write, got {other:?}").into()),
    }
}

/// What a publication leaves in a world: its App check's key, status and last update, the
/// summary bodies and the evidence recorded.
type Effects = (
    String,
    CheckStatus,
    Option<CheckUpdate>,
    Vec<String>,
    Vec<EvidenceEntry>,
);

/// The effects `world` holds.
fn effects(world: &FakeWorld) -> Result<Effects, Box<dyn Error>> {
    let FakeCheck {
        external_id,
        status,
        output,
        ..
    } = world.app_check(NAME)?;
    Ok((
        external_id,
        status,
        output,
        world
            .comments
            .borrow()
            .iter()
            .map(|comment| comment.body.clone())
            .collect(),
        world.evidence.borrow().clone(),
    ))
}

/// The effects of `publish` from memory on a stamped world, its upload failing when `fails`.
fn in_memory(run: &Run, fails: bool) -> Result<Effects, Box<dyn Error>> {
    let world = stamped()?;
    if fails {
        world.fail(Call::Upload, 1);
    }
    written(run.publish(&world))?;
    effects(&world)
}

/// A PR changing a CI workflow (a trust path), overridden with `all` so the rubric alone would
/// publish `neutral`.
fn trust_path_run() -> Result<Run, Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.changes = assay_app::ChangedFiles::Listed(vec![FileChange::new(
        ".github/workflows/ci.yml",
        Lines::Counted {
            added: 1,
            removed: 0,
        },
    )]);
    let mut run = Run::of(&Scenario::of(pr)?)?;
    let reasons_digest = decide(&run.rubric, &run.inputs).reasons_digest();
    run.inputs.override_record = Some(Override {
        scope: OverrideScope::All,
        reasons_digest,
        by: "owner".to_owned(),
    });
    assert_eq!(
        run.inputs.pr.classes,
        [PrClass::TrustPath],
        "the run is trust path"
    );
    Ok(run)
}

/// The default run with an approval of the head given before its push, which the summary lists
/// as ignored.
fn ignored_approval_run() -> Result<Run, Box<dyn Error>> {
    let mut run = Run::new()?;
    let mut pr = World::opened_by(CONTRIBUTOR);
    pr.commit(HEAD_COMMIT, &[FIRST], By::user(CONTRIBUTOR));
    pr.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 4));
    pr.push((FIRST, HEAD_COMMIT), CONTRIBUTOR, 5);
    pr.head(HEAD_COMMIT);
    run.github = pr.state()?;
    assert_eq!(
        run.github.ignored_approvals().count(),
        1,
        "the approval predates the push"
    );
    Ok(run)
}

/// `bundle` with `change` applied to its JSON, and the new bytes' digest.
fn edited(
    bundle: &PreparedBundle,
    change: impl FnOnce(&mut Value),
) -> Result<(Vec<u8>, Sha256Digest), Box<dyn Error>> {
    let mut value: Value = serde_json::from_slice(bundle.bytes())?;
    change(&mut value);
    let bytes = serde_json::to_vec(&value)?;
    let digest = Sha256Digest::of(&bytes);
    Ok((bytes, digest))
}

// --- prepare ---

#[test]
fn prepare_gives_the_bundle_the_in_memory_path_uploads_and_its_sha256() -> Result<(), Box<dyn Error>>
{
    let run = Run::new()?;
    let world = stamped()?;
    written(run.publish(&world))?;

    let bundle = prepared(&run)?;

    let uploads = world.uploads.borrow();
    let [(_, file, uploaded)] = uploads.as_slice() else {
        return Err("one upload".into());
    };
    assert_eq!(
        (file.as_str(), bundle.bytes()),
        (BUNDLE_FILE, uploaded.as_slice())
    );
    assert_eq!(bundle.digest(), &Sha256Digest::of(uploaded));
    Ok(())
}

#[test]
fn prepare_refuses_inputs_it_could_not_publish_from_the_bundle() -> Result<(), Box<dyn Error>> {
    let mut not_a_digest = Run::new()?;
    "pending:7:3".clone_into(&mut not_a_digest.inputs.decision_key);
    let mut other_classes = Run::new()?;
    other_classes.inputs.pr.classes = vec![PrClass::TrustPath];
    let mut other_tier = Run::new()?;
    other_tier.inputs.pr.tier = Tier::Full;
    let mut other_base = Run::new()?;
    "release".clone_into(&mut other_base.inputs.pr.base_ref);

    assert_matches!(prepared(&not_a_digest), Err(PrepareError::DecisionKey));
    assert_matches!(prepared(&other_classes), Err(PrepareError::Classes));
    assert_matches!(prepared(&other_tier), Err(PrepareError::Tier));
    assert_matches!(prepared(&other_base), Err(PrepareError::BaseRef));
    Ok(())
}

// --- a confirmed upload ---

#[test]
fn a_confirmed_upload_publishes_what_the_in_memory_path_publishes() -> Result<(), Box<dyn Error>> {
    for run in [Run::new()?, trust_path_run()?, ignored_approval_run()?] {
        let expected = in_memory(&run, false)?;
        let bundle = prepared(&run)?;
        let (world, bundles) = (
            stamped()?,
            FakeBundles::new(Stored::Bytes(bundle.bytes().to_vec())),
        );

        let written = persisted_written(publish_prepared(
            &run,
            PersistOutcome::Success,
            (&world, &bundles),
        )?)?;

        assert_matches!(written.persist, None);
        assert_eq!(effects(&world)?, expected);
        assert_eq!(
            bundles.asked.borrow().as_slice(),
            [(
                run.request.plan.run_uid,
                BUNDLE_FILE.to_owned(),
                MAX_BUNDLE_BYTES
            )]
        );
    }
    Ok(())
}

#[test]
fn a_check_already_completed_under_the_key_is_left_without_reading_the_store()
-> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    written(run.publish(&world))?;
    let bundles = FakeBundles::new(Stored::Missing);

    let outcome = publish_prepared(&run, PersistOutcome::Success, (&world, &bundles))?;

    assert_matches!(outcome, Ok(Published::Unchanged));
    assert_eq!(bundles.asked.borrow().len(), 0);
    Ok(())
}

// --- an upload that did not succeed ---

#[test]
fn an_upload_that_did_not_succeed_fails_a_trust_path_pr_closed_unread() -> Result<(), Box<dyn Error>>
{
    let run = trust_path_run()?;
    let expected = in_memory(&run, true)?;
    for upload in [
        PersistOutcome::Failure,
        PersistOutcome::Cancelled,
        PersistOutcome::Skipped,
    ] {
        let (world, bundles) = (stamped()?, FakeBundles::new(Stored::Missing));

        let written = persisted_written(publish_prepared(&run, upload, (&world, &bundles))?)?;

        assert_matches!(
            written.persist,
            Some(PersistFailure::Upload(Unconfirmed::NotUploaded(outcome))) if outcome == upload
        );
        assert_eq!(
            written.state,
            CheckState::Completed(CheckConclusion::Failure)
        );
        assert_eq!(effects(&world)?, expected, "persist-error, no evidence");
        assert_eq!(bundles.asked.borrow().len(), 0, "the store is not read");
    }
    Ok(())
}

#[test]
fn an_upload_that_did_not_succeed_only_warns_on_another_pr() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let expected = in_memory(&run, true)?;
    let (world, bundles) = (stamped()?, FakeBundles::new(Stored::Missing));

    let written = persisted_written(publish_prepared(
        &run,
        PersistOutcome::Failure,
        (&world, &bundles),
    )?)?;

    assert_matches!(
        written.persist,
        Some(PersistFailure::Upload(Unconfirmed::NotUploaded(
            PersistOutcome::Failure
        )))
    );
    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Success)
    );
    assert_eq!(effects(&world)?, expected, "a warning, no evidence");
    assert_eq!(bundles.asked.borrow().len(), 0, "the store is not read");
    Ok(())
}

#[test]
fn a_reported_upload_the_store_does_not_confirm_is_not_persisted() -> Result<(), Box<dyn Error>> {
    let run = trust_path_run()?;
    let expected = in_memory(&run, true)?;
    let mut other = prepared(&run)?.bytes().to_vec();
    other.push(b'\n');
    let cases = [
        Stored::Missing,
        Stored::Expired,
        Stored::TooLarge,
        Stored::Fails,
        Stored::Bytes(other),
    ];
    for stored in cases {
        let (world, bundles) = (stamped()?, FakeBundles::new(stored.clone()));

        let written = persisted_written(publish_prepared(
            &run,
            PersistOutcome::Success,
            (&world, &bundles),
        )?)?;

        let unconfirmed = match written.persist {
            Some(PersistFailure::Upload(unconfirmed)) => unconfirmed,
            other => {
                return Err(
                    format!("{stored:?}: expected an upload failure, got {other:?}").into(),
                );
            }
        };
        match stored {
            Stored::Missing => assert_matches!(unconfirmed, Unconfirmed::Missing),
            Stored::Expired => assert_matches!(unconfirmed, Unconfirmed::Expired),
            Stored::TooLarge => assert_matches!(unconfirmed, Unconfirmed::TooLarge),
            Stored::Fails => assert_matches!(unconfirmed, Unconfirmed::Fetch(FakeError)),
            Stored::Bytes(_) => assert_matches!(unconfirmed, Unconfirmed::Digest),
        }
        assert_eq!(effects(&world)?, expected, "persist-error, no evidence");
    }
    Ok(())
}

// --- a bundle that is not this run's decision ---

/// `bundle` published for `run` at `target`, with nothing read or written allowed.
fn refused(
    run: &Run,
    target: Target<'_>,
    (bytes, digest): (&[u8], &Sha256Digest),
) -> Result<BundleRefused, Box<dyn Error>> {
    let (world, bundles) = (stamped()?, FakeBundles::new(Stored::Missing));
    let outcome = publish_at(
        &persisted(run, target, (bytes, digest), PersistOutcome::Success),
        (&world, &bundles),
    );
    assert_eq!(world.calls.borrow().as_slice(), [], "no port call");
    assert_eq!(bundles.asked.borrow().len(), 0, "no store read");
    match outcome {
        Err(PersistedError::Bundle(refusal)) => Ok(refusal),
        other => Err(format!("expected a refused bundle, got {other:?}").into()),
    }
}

#[test]
fn a_bundle_that_does_not_hash_to_its_digest_is_refused_before_any_port_call()
-> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let bundle = prepared(&run)?;
    let other = Sha256Digest::of(b"another bundle");

    let refusal = refused(&run, run.target(), (bundle.bytes(), &other))?;

    assert_matches!(refusal, BundleRefused::Digest);
    Ok(())
}

#[test]
fn a_bundle_of_another_run_head_or_base_is_refused_before_any_port_call()
-> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let bundle = prepared(&run)?;
    let parts = (bundle.bytes(), bundle.digest());
    let mut other_run = run.target();
    other_run.run_uid.run_id = other_run.run_uid.run_id.saturating_add(1);
    let mut retry = run.target();
    retry.run_uid.run_attempt = retry.run_uid.run_attempt.saturating_add(1);
    let mut moved = run.request.plan.pull_request.clone();
    moved.head_sha = "c".repeat(40).parse()?;
    let mut rebased = run.request.plan.pull_request.clone();
    "release".clone_into(&mut rebased.base_ref);
    let at = |pull_request| Target {
        run_uid: run.request.plan.run_uid,
        pull_request,
        default_branch: DEFAULT_BRANCH,
    };

    assert_matches!(
        refused(&run, other_run, parts)?,
        BundleRefused::Run(RunField::RunId)
    );
    assert_matches!(refused(&run, retry, parts)?, BundleRefused::NotFirstAttempt);
    assert_matches!(refused(&run, at(&moved), parts)?, BundleRefused::Head);
    assert_matches!(refused(&run, at(&rebased), parts)?, BundleRefused::BaseRef);
    Ok(())
}

#[test]
fn a_bundle_whose_decision_or_rubric_does_not_recompute_is_refused() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let bundle = prepared(&run)?;
    let (failed, failed_digest) = edited(&bundle, |value| {
        value["decision"]["conclusion"] = Value::from("failure");
    })?;
    let (rubric, rubric_digest) = edited(&bundle, |value| {
        value["rubric_version"] = Value::from("0");
    })?;
    let (tampered, tampered_digest) = edited(&bundle, |value| {
        value["documents"][0]["text"] = Value::from("{}");
    })?;
    let (not_bundle, not_bundle_digest) = edited(&bundle, |value| {
        value["schema"] = Value::from("assay.run-inputs/2");
    })?;

    assert_matches!(
        refused(&run, run.target(), (&failed, &failed_digest))?,
        BundleRefused::Decision(_)
    );
    assert_matches!(
        refused(&run, run.target(), (&rubric, &rubric_digest))?,
        BundleRefused::RubricVersion
    );
    assert_matches!(
        refused(&run, run.target(), (&tampered, &tampered_digest))?,
        BundleRefused::Document { index: 0 }
    );
    assert_matches!(
        refused(&run, run.target(), (&not_bundle, &not_bundle_digest))?,
        BundleRefused::Unreadable(_)
    );
    Ok(())
}

// --- the upload step's outcome ---

#[test]
fn the_upload_outcome_reads_only_the_four_step_outcomes() {
    let read = str::parse::<PersistOutcome>;

    assert_eq!(
        ["success", "failure", "cancelled", "skipped"].map(read),
        [
            Ok(PersistOutcome::Success),
            Ok(PersistOutcome::Failure),
            Ok(PersistOutcome::Cancelled),
            Ok(PersistOutcome::Skipped)
        ]
    );
    assert_eq!(
        ["Success", "", "success ", "neutral"].map(read),
        [Err(UnknownPersistOutcome); 4]
    );
}
