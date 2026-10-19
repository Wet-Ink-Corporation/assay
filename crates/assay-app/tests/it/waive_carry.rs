//! A waiver lives for one head and is carried to a new head only when its finding fingerprint
//! and gate-document digests are unchanged (04 §5 lifetime table, R3-13, #59): what a run binds
//! a waiver to, what the previous run's bundle says it waived, and which waivers a new head
//! carries.

use std::collections::BTreeSet;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    ArtifactJob, AuthenticatedRun, BUNDLE_FILE, Carry, CarryReset, EvidencePointer, GitHubState,
    PriorCarry, PriorWaivers, Recorded, Sha256Digest, WaiverBindings, prior_waivers,
};
use assay_domain::{Fingerprint, GateConclusion, Waiver};
use serde_json::{Value, json};

use crate::decide_fakes::{Scenario, authenticated, gate_document, gate_finding};
use crate::eligibility_fakes::{APPROVER, By, CONTRIBUTOR, World, comment, created};
use crate::publish_support::{HEAD_COMMIT, Run, stamped};
use crate::replay_fakes::{FakeBundles, Stored};
use crate::verify_fakes::REPOSITORY;

/// The commit pushed after the waived head.
const NEXT: char = 'c';

/// A temper document of `version` concluding `fail` with `findings`.
fn temper_document(version: &str, findings: &[Value]) -> Vec<u8> {
    json!({
        "schema": "assay.findings/1",
        "source": {"kind": "gate", "name": "temper", "version": version, "run_key": "run-42-1"},
        "conclusion": "fail",
        "findings": findings,
    })
    .to_string()
    .into_bytes()
}

/// The digest of the scenario's passing lint temper document.
fn lint_temper() -> Sha256Digest {
    Sha256Digest::of(&gate_document("temper", "pass", &json!([])))
}

/// The `raised` finding on the head's one line, which rubric v1 lets an owner waive.
fn raised() -> Value {
    gate_finding("raised", "src/lib.rs", 1, "pub fn two() {}")
}

/// A run whose static temper gate fails with `document`.
fn run_with(document: Vec<u8>) -> Result<AuthenticatedRun, Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Fail,
        document,
    )?;
    authenticated(scenario.authenticate()?)
}

/// The fingerprint of the static temper gate's first finding.
fn first_fp(run: &AuthenticatedRun) -> Result<Fingerprint, Box<dyn Error>> {
    Ok(run
        .gates()
        .first()
        .and_then(|gate| gate.findings.first())
        .ok_or("the static gate has a finding")?
        .fp)
}

/// The bundle of a run over `document` that applied `waivers`, and its evidence pointer.
fn prior_bundle(
    document: Vec<u8>,
    waivers: impl FnOnce(Fingerprint) -> Vec<Waiver>,
) -> Result<(Run, Vec<u8>, EvidencePointer), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Fail,
        document,
    )?;
    let mut run = Run::of(&scenario)?;
    run.inputs.waivers = waivers(first_fp(&run.authenticated)?);
    let world = stamped()?;
    run.publish(&world)?;
    let bytes = {
        let uploads = world.uploads.borrow();
        let [(_, file, bytes)] = uploads.as_slice() else {
            return Err(format!("expected one upload, got {}", uploads.len()).into());
        };
        assert_eq!(file, BUNDLE_FILE, "the bundle file");
        bytes.clone()
    };
    let pointer = EvidencePointer {
        run_id: run.request.plan.run_uid.run_id,
        run_attempt: NonZeroU64::MIN,
        sha256: Sha256Digest::of(&bytes),
    };
    Ok((run, bytes, pointer))
}

/// What `prior_waivers` reads for `run` through `pointer` from `stored`, and the store.
fn read_prior(
    run: &Run,
    carry: Carry,
    pointer: &EvidencePointer,
    stored: Stored,
) -> Result<(PriorWaivers, FakeBundles), Box<dyn Error>> {
    let source = FakeBundles::new(stored);
    let request = PriorCarry {
        repo_id: run.request.plan.run_uid.repo_id,
        pointer,
        repository: REPOSITORY,
    };
    let prior = prior_waivers(carry, &request, &source)?;
    Ok((prior, source))
}

/// The approver waived `fp` on the scenario's head; then the contributor pushed [`NEXT`],
/// pushed by `pusher`.
fn waived_then_pushed(fp: Fingerprint, pusher: u64) -> Result<GitHubState, Box<dyn Error>> {
    let mut world = World::opened_at(CONTRIBUTOR, HEAD_COMMIT);
    let posted = comment(40, APPROVER, 5, &format!("/assay waive {fp}"));
    let Recorded::Record(record) = world.state()?.record_command(&created(posted.clone()))? else {
        return Err("a waive is recorded".into());
    };
    world.commands.push(record);
    world.comments.insert(posted.id, posted);
    world.commit(NEXT, &[HEAD_COMMIT], By::user(CONTRIBUTOR));
    world.push((HEAD_COMMIT, NEXT), pusher, 7);
    world.head(NEXT);
    Ok(world.state()?)
}

/// The approver's waiver of `fp`.
fn by_approver(fp: Fingerprint) -> Vec<Waiver> {
    vec![Waiver {
        fp,
        by: "approver".to_owned(),
    }]
}

#[test]
fn a_run_binds_each_gate_fingerprint_to_the_documents_carrying_it() -> Result<(), Box<dyn Error>> {
    let document = temper_document("git:x", &[raised()]);
    let digest = Sha256Digest::of(&document);
    let run = run_with(document)?;
    let fp = first_fp(&run)?;

    let bindings: WaiverBindings = run.waiver_bindings();

    assert_eq!(
        bindings.digests(fp),
        Some(&BTreeSet::from([digest, lint_temper()])),
        "both temper documents: a gate is named, not its job"
    );
    Ok(())
}

#[test]
fn the_previous_bundle_names_what_its_decision_waived_and_its_bindings()
-> Result<(), Box<dyn Error>> {
    let document = temper_document("git:x", &[raised()]);
    let digest = Sha256Digest::of(&document);
    let (run, bytes, pointer) = prior_bundle(document, by_approver)?;
    let fp = first_fp(&run.authenticated)?;

    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;

    assert_eq!(prior.fingerprints().collect::<Vec<_>>(), [fp]);
    assert_eq!(
        prior.digests(fp),
        Some(&BTreeSet::from([digest, lint_temper()]))
    );
    Ok(())
}

#[test]
fn a_waiver_is_not_carried_once_another_document_of_its_gate_changed() -> Result<(), Box<dyn Error>>
{
    let document = temper_document("git:x", &[raised()]);
    let (run, bytes, pointer) = prior_bundle(document.clone(), by_approver)?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;
    let mut scenario = Scenario::new()?;
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Fail,
        document,
    )?;
    scenario.publish_gate(
        (ArtifactJob::GatesLint, "temper"),
        GateConclusion::Pass,
        temper_document("git:y", &[]),
    )?;
    let next = authenticated(scenario.authenticate()?)?;
    let state = waived_then_pushed(first_fp(&next)?, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, [], "the lint job's temper document changed");
    Ok(())
}

#[test]
fn a_previous_run_that_waived_nothing_carries_nothing() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) =
        prior_bundle(temper_document("git:x", &[raised()]), |_| Vec::new())?;

    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;

    assert_eq!(prior.fingerprints().count(), 0);
    Ok(())
}

#[test]
fn a_reset_or_first_review_reads_no_bundle() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = prior_bundle(temper_document("git:x", &[raised()]), by_approver)?;

    for carry in [Carry::FirstReview, Carry::Reset(CarryReset::Rewritten)] {
        let (prior, source) = read_prior(&run, carry, &pointer, Stored::Bytes(bytes.clone()))?;

        assert_eq!(prior.fingerprints().count(), 0, "{carry:?}");
        assert_eq!(source.asked.borrow().len(), 0, "{carry:?}: nothing fetched");
    }
    Ok(())
}

#[test]
fn a_waiver_is_carried_when_its_fingerprint_and_documents_are_unchanged()
-> Result<(), Box<dyn Error>> {
    let document = temper_document("git:x", &[raised()]);
    let (run, bytes, pointer) = prior_bundle(document.clone(), by_approver)?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;
    let next = run_with(document)?;
    let fp = first_fp(&next)?;
    let state = waived_then_pushed(fp, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(
        state.waivers(),
        [],
        "the record itself lapsed on the new head"
    );
    assert_eq!(waivers, by_approver(fp));
    Ok(())
}

#[test]
fn a_waiver_is_not_carried_once_its_gate_document_changed() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = prior_bundle(temper_document("git:x", &[raised()]), by_approver)?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;
    let next = run_with(temper_document("git:y", &[raised()]))?;
    let fp = first_fp(&next)?;
    assert_eq!(
        first_fp(&run.authenticated)?,
        fp,
        "the finding itself is unchanged"
    );
    let state = waived_then_pushed(fp, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, []);
    Ok(())
}

#[test]
fn a_waiver_the_previous_decision_did_not_apply_is_not_carried() -> Result<(), Box<dyn Error>> {
    let document = temper_document("git:x", &[raised()]);
    let (run, bytes, pointer) = prior_bundle(document.clone(), |_| Vec::new())?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;
    let next = run_with(document)?;
    let state = waived_then_pushed(first_fp(&next)?, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, []);
    Ok(())
}

#[test]
fn a_waiver_whose_owner_pushed_the_new_head_is_not_carried() -> Result<(), Box<dyn Error>> {
    let document = temper_document("git:x", &[raised()]);
    let (run, bytes, pointer) = prior_bundle(document.clone(), by_approver)?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;
    let next = run_with(document)?;
    let state = waived_then_pushed(first_fp(&next)?, APPROVER)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, [], "a pusher of the PR is no longer eligible");
    Ok(())
}

#[test]
fn a_waiver_whose_comment_was_deleted_is_not_carried() -> Result<(), Box<dyn Error>> {
    let document = temper_document("git:x", &[raised()]);
    let (run, bytes, pointer) = prior_bundle(document.clone(), by_approver)?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;
    let next = run_with(document)?;
    let fp = first_fp(&next)?;
    let mut world = World::opened_at(CONTRIBUTOR, HEAD_COMMIT);
    let posted = comment(40, APPROVER, 5, &format!("/assay waive {fp}"));
    let Recorded::Record(record) = world.state()?.record_command(&created(posted))? else {
        return Err("a waive is recorded".into());
    };
    world.commands.push(record);
    world.commit(NEXT, &[HEAD_COMMIT], By::user(CONTRIBUTOR));
    world.push((HEAD_COMMIT, NEXT), CONTRIBUTOR, 7);
    world.head(NEXT);
    let state = world.state()?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, []);
    Ok(())
}

#[test]
fn a_waiver_on_the_decided_head_is_kept_once_and_not_carried_again() -> Result<(), Box<dyn Error>> {
    let document = temper_document("git:x", &[raised()]);
    let (run, bytes, pointer) = prior_bundle(document.clone(), by_approver)?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes))?;
    let next = run_with(document)?;
    let fp = first_fp(&next)?;
    let mut world = World::opened_at(CONTRIBUTOR, HEAD_COMMIT);
    let posted = comment(40, APPROVER, 5, &format!("/assay waive {fp}"));
    let Recorded::Record(record) = world.state()?.record_command(&created(posted.clone()))? else {
        return Err("a waive is recorded".into());
    };
    world.commands.push(record);
    world.comments.insert(posted.id, posted);
    let state = world.state()?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, by_approver(fp));
    Ok(())
}
