//! A waiver lives for one head and is carried to a new head only when its finding fingerprint
//! and gate-document digests are unchanged (04 §5 lifetime table, R3-13, #59): what a run binds
//! a waiver to, what the previous run's bundle says it waived, and which waivers a new head
//! carries. Every head's gate documents carry that head's own `source.run_key`, as the real
//! adapters write them, so a carry here crosses the contract's per-run field (wi-bc5341).

use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    ArtifactJob, AuthenticatedRun, BUNDLE_FILE, BundleShape, Carry, CarryReset, EvidencePointer,
    GitHubState, PriorCarry, PriorCarryError, PriorWaivers, Recorded, Sha256Digest, prior_waivers,
};
use assay_domain::{Fingerprint, GateConclusion, Waiver};
use serde_json::{Value, json};

use crate::decide_fakes::{Scenario, authenticated, gate_finding};
use crate::eligibility_fakes::{APPROVER, By, CONTRIBUTOR, World, comment, created};
use crate::plan_fakes::FakeError;
use crate::publish_support::{HEAD_COMMIT, Run, stamped};
use crate::replay_fakes::{FakeBundles, Stored};
use crate::verify_fakes::REPOSITORY;

/// The commit pushed after the waived head.
const NEXT: char = 'c';
/// The evidence key the waived head's gates wrote.
const PRIOR_KEY: &str = "run-42-1";
/// The evidence key the next head's gates wrote.
const NEXT_KEY: &str = "run-43-1";

/// A temper document of `version` for run `run_key`, concluding `conclusion` with `findings`.
fn temper_document(version: &str, run_key: &str, conclusion: &str, findings: &[Value]) -> Vec<u8> {
    json!({
        "schema": "assay.findings/1",
        "source": {"kind": "gate", "name": "temper", "version": version, "run_key": run_key},
        "conclusion": conclusion,
        "findings": findings,
    })
    .to_string()
    .into_bytes()
}

/// The same document as [`temper_document`] for one `raised` finding, written with its keys in
/// another order and other spacing, as another serialiser might.
fn reordered_raised(run_key: &str) -> Vec<u8> {
    let finding = raised().to_string();
    format!(
        r#"{{ "findings": [{finding}], "conclusion": "fail",
            "source": {{"run_key": "{run_key}", "version": "git:x", "name": "temper", "kind": "gate"}},
            "schema": "assay.findings/1" }}"#
    )
    .into_bytes()
}

/// The `raised` finding on the head's one line, which rubric v1 lets an owner waive.
fn raised() -> Value {
    gate_finding("raised", "src/lib.rs", 1, "pub fn two() {}")
}

/// An `exception` finding on the same line: a second human-decision finding.
fn exception() -> Value {
    gate_finding("exception", "src/lib.rs", 1, "pub fn two() {}")
}

/// The static temper document of run `run_key` failing on [`raised`] alone.
fn raised_document(run_key: &str) -> Vec<u8> {
    temper_document("git:x", run_key, "fail", &[raised()])
}

/// A scenario whose static temper gate concludes `conclusion` with `document` and whose lint
/// temper gate passes, both written by run `run_key`.
fn scenario_with(
    document: Vec<u8>,
    conclusion: GateConclusion,
    run_key: &str,
) -> Result<Scenario, Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate((ArtifactJob::GatesStatic, "temper"), conclusion, document)?;
    scenario.publish_gate(
        (ArtifactJob::GatesLint, "temper"),
        GateConclusion::Pass,
        temper_document("git:x", run_key, "pass", &[]),
    )?;
    Ok(scenario)
}

/// A scenario whose static temper gate fails on [`raised`] alone, which an owner may waive.
pub(crate) fn raised_scenario() -> Result<Scenario, Box<dyn Error>> {
    scenario_with(raised_document(PRIOR_KEY), GateConclusion::Fail, PRIOR_KEY)
}

/// The next head's run, its static temper gate failing with `document`.
fn next_run(document: Vec<u8>) -> Result<AuthenticatedRun, Box<dyn Error>> {
    authenticated(scenario_with(document, GateConclusion::Fail, NEXT_KEY)?.authenticate()?)
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

/// The bundle of the waived head's run over [`raised_document`] that applied `waivers`, and
/// its evidence pointer.
pub(crate) fn prior_bundle(
    waivers: impl FnOnce(Fingerprint) -> Vec<Waiver>,
) -> Result<(Run, Vec<u8>, EvidencePointer), Box<dyn Error>> {
    let scenario = scenario_with(raised_document(PRIOR_KEY), GateConclusion::Fail, PRIOR_KEY)?;
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
    let pointer = pointer_to(&run, &bytes);
    Ok((run, bytes, pointer))
}

/// The evidence pointer of `run` to `bytes`.
fn pointer_to(run: &Run, bytes: &[u8]) -> EvidencePointer {
    EvidencePointer {
        run_id: run.request.plan.run_uid.run_id,
        run_attempt: NonZeroU64::MIN,
        sha256: Sha256Digest::of(bytes),
    }
}

/// What `prior_waivers` reads for `run` through `pointer` from `stored`, and the store.
fn read_prior(
    run: &Run,
    carry: Carry,
    pointer: &EvidencePointer,
    stored: Stored,
) -> (
    Result<PriorWaivers, PriorCarryError<FakeError>>,
    FakeBundles,
) {
    let source = FakeBundles::new(stored);
    let request = PriorCarry {
        repo_id: run.request.plan.run_uid.repo_id,
        pointer,
        repository: REPOSITORY,
    };
    (prior_waivers(carry, &request, &source), source)
}

/// What the waived head's bundle says it waived: the approver's waiver of [`raised`].
fn waived_prior() -> Result<(Run, PriorWaivers), Box<dyn Error>> {
    let (run, bytes, pointer) = prior_bundle(by_approver)?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));
    Ok((run, prior?))
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
pub(crate) fn by_approver(fp: Fingerprint) -> Vec<Waiver> {
    vec![Waiver {
        fp,
        by: "approver".to_owned(),
    }]
}

// --- what a waiver is bound to ---

#[test]
fn a_binding_ignores_the_run_key_and_the_documents_layout() -> Result<(), Box<dyn Error>> {
    let prior = authenticated(
        scenario_with(raised_document(PRIOR_KEY), GateConclusion::Fail, PRIOR_KEY)?
            .authenticate()?,
    )?;
    let next = next_run(reordered_raised(NEXT_KEY))?;
    let fp = first_fp(&prior)?;
    assert_eq!(first_fp(&next)?, fp, "one finding on both heads");

    let (before, after) = (prior.waiver_bindings(), next.waiver_bindings());

    let bound = before.digests(fp).ok_or("the finding is bound")?;
    assert_eq!(
        bound.len(),
        2,
        "both temper documents: a gate is named, not its job"
    );
    assert_eq!(after.digests(fp), Some(bound));
    Ok(())
}

#[test]
fn a_binding_changes_with_the_documents_conclusion() -> Result<(), Box<dyn Error>> {
    let failing = next_run(raised_document(NEXT_KEY))?;
    let neutral = authenticated(
        scenario_with(
            temper_document("git:x", NEXT_KEY, "neutral", &[raised()]),
            GateConclusion::Neutral,
            NEXT_KEY,
        )?
        .authenticate()?,
    )?;
    let fp = first_fp(&failing)?;
    assert_eq!(first_fp(&neutral)?, fp, "the finding itself is unchanged");

    assert_ne!(
        failing.waiver_bindings().digests(fp),
        neutral.waiver_bindings().digests(fp)
    );
    Ok(())
}

// --- what the previous run's bundle says ---

#[test]
fn the_previous_bundle_names_what_its_decision_waived_and_its_bindings()
-> Result<(), Box<dyn Error>> {
    let (run, prior) = waived_prior()?;
    let fp = first_fp(&run.authenticated)?;

    assert_eq!(prior.fingerprints().collect::<Vec<_>>(), [fp]);
    assert_eq!(
        prior.digests(fp),
        run.authenticated.waiver_bindings().digests(fp),
        "the bundle binds as the run did"
    );
    Ok(())
}

#[test]
fn a_previous_run_that_waived_nothing_carries_nothing() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = prior_bundle(|_| Vec::new())?;

    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    assert_eq!(prior?.fingerprints().count(), 0);
    Ok(())
}

#[test]
fn a_reset_or_first_review_reads_no_bundle() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = prior_bundle(by_approver)?;

    for carry in [Carry::FirstReview, Carry::Reset(CarryReset::Rewritten)] {
        let (prior, source) = read_prior(&run, carry, &pointer, Stored::Bytes(bytes.clone()));

        assert_eq!(prior?.fingerprints().count(), 0, "{carry:?}");
        assert_eq!(source.asked.borrow().len(), 0, "{carry:?}: nothing fetched");
    }
    Ok(())
}

#[test]
fn a_bundle_whose_decision_is_not_a_decision_is_refused() -> Result<(), Box<dyn Error>> {
    let (run, bytes, _pointer) = prior_bundle(by_approver)?;
    let mut bundle: Value = serde_json::from_slice(&bytes)?;
    *bundle.get_mut("decision").ok_or("a decision")? = json!({"schema": "assay.other/1"});
    let tampered = serde_json::to_vec(&bundle)?;
    let pointer = pointer_to(&run, &tampered);

    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(tampered));

    assert!(
        matches!(prior, Err(PriorCarryError::Shape(BundleShape::Shape))),
        "{prior:?}"
    );
    Ok(())
}

// --- which waivers a new head carries ---

#[test]
fn a_waiver_is_carried_to_a_head_whose_gate_wrote_its_own_run_key() -> Result<(), Box<dyn Error>> {
    let (_run, prior) = waived_prior()?;
    let next = next_run(raised_document(NEXT_KEY))?;
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
fn a_waiver_is_not_carried_once_its_gate_reports_another_finding() -> Result<(), Box<dyn Error>> {
    let (run, prior) = waived_prior()?;
    let next = next_run(temper_document(
        "git:x",
        NEXT_KEY,
        "fail",
        &[raised(), exception()],
    ))?;
    let fp = first_fp(&next)?;
    assert_eq!(
        first_fp(&run.authenticated)?,
        fp,
        "the waived finding is unchanged"
    );
    let state = waived_then_pushed(fp, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, []);
    Ok(())
}

#[test]
fn a_waiver_is_not_carried_once_its_gate_document_changed() -> Result<(), Box<dyn Error>> {
    let (run, prior) = waived_prior()?;
    let next = next_run(temper_document("git:y", NEXT_KEY, "fail", &[raised()]))?;
    let fp = first_fp(&next)?;
    assert_eq!(
        first_fp(&run.authenticated)?,
        fp,
        "the finding itself is unchanged"
    );
    let state = waived_then_pushed(fp, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, [], "the gate's version changed");
    Ok(())
}

#[test]
fn a_waiver_is_not_carried_once_another_document_of_its_gate_changed() -> Result<(), Box<dyn Error>>
{
    let (_run, prior) = waived_prior()?;
    let mut scenario = scenario_with(raised_document(NEXT_KEY), GateConclusion::Fail, NEXT_KEY)?;
    scenario.publish_gate(
        (ArtifactJob::GatesLint, "temper"),
        GateConclusion::Pass,
        temper_document("git:y", NEXT_KEY, "pass", &[]),
    )?;
    let next = authenticated(scenario.authenticate()?)?;
    let state = waived_then_pushed(first_fp(&next)?, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, [], "the lint job's temper document changed");
    Ok(())
}

#[test]
fn a_waiver_the_previous_decision_did_not_apply_is_not_carried() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = prior_bundle(|_| Vec::new())?;
    let (prior, _source) = read_prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));
    let next = next_run(raised_document(NEXT_KEY))?;
    let state = waived_then_pushed(first_fp(&next)?, CONTRIBUTOR)?;

    let waivers = state.waivers_carried(&prior?, &next.waiver_bindings());

    assert_eq!(waivers, []);
    Ok(())
}

#[test]
fn a_waiver_whose_owner_pushed_the_new_head_is_not_carried() -> Result<(), Box<dyn Error>> {
    let (_run, prior) = waived_prior()?;
    let next = next_run(raised_document(NEXT_KEY))?;
    let state = waived_then_pushed(first_fp(&next)?, APPROVER)?;

    let waivers = state.waivers_carried(&prior, &next.waiver_bindings());

    assert_eq!(waivers, [], "a pusher of the PR is no longer eligible");
    Ok(())
}

#[test]
fn a_waiver_whose_comment_was_deleted_is_not_carried() -> Result<(), Box<dyn Error>> {
    let (_run, prior) = waived_prior()?;
    let next = next_run(raised_document(NEXT_KEY))?;
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
    let (_run, prior) = waived_prior()?;
    let next = next_run(raised_document(NEXT_KEY))?;
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
