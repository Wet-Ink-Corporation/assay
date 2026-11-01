//! decide-only's decision inputs (D15, QA-08): everything but the snapshot's facts is the
//! evidence bundle's, and the decision key is composed by the one rule `decide` uses.

use std::error::Error;

use assay_app::{
    AdmitJob, AdmitOutcome, EvidenceBundle, GitHubState, InputsError, InputsRequest, JobResult,
    PriorWaivers, RedecisionRequest, ReviewJob, ReviewStage, ReviewState, TierDegrade,
    decision_inputs, redecision_inputs,
};
use assay_domain::{
    Aeu, Conclusion, DecisionInputs, OwnerApproval, PrClass, Readiness, ReasonCode, Tier, decide,
};
use proptest::collection::vec;
use proptest::prelude::*;

use crate::eligibility_fakes::{
    APPROVER, By, CONTRIBUTOR, DAILY, OWNER, World, comment, created, lists,
};
use crate::publish_support::HEAD_COMMIT;
use crate::redecide_fakes::{evidence, evidence_of, redecided};
use crate::support::rubric_v1;
use crate::waive_carry::{by_approver, raised_scenario};

/// A contributor's PR on the scenario's head, pushed when it was opened (minute 0).
fn contributor_pr() -> World {
    World::opened_at(CONTRIBUTOR, HEAD_COMMIT)
}

/// The inputs decide-only composes from `evidence` and `state`, with `requeues`.
fn inputs(
    evidence: &EvidenceBundle,
    state: &GitHubState,
    requeues: u32,
) -> Result<DecisionInputs, InputsError> {
    redecision_inputs(&RedecisionRequest {
        evidence,
        state,
        requeues,
    })
}

#[test]
fn an_approval_after_the_push_clock_turns_trust_path_unapproved_into_success()
-> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    let evidence = evidence(&world, |run| {
        run.inputs.pr.classes = vec![PrClass::TrustPath];
        Ok(())
    })?;
    let recorded = evidence.decision();
    assert_eq!(recorded.conclusion, Conclusion::Failure);
    assert!(
        recorded
            .reasons
            .iter()
            .any(|reason| reason.code == ReasonCode::TrustPathUnapproved),
        "the evidence decision is trust-path-unapproved: {:?}",
        recorded.reasons
    );
    world.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 5));
    let state = redecided(&world, &evidence, &lists())?;

    let redecision = inputs(&evidence, &state, 0)?;

    assert_eq!(redecision.pr.owner_approval, OwnerApproval::Approved);
    let decision = decide(&rubric_v1()?, &redecision);
    assert_eq!(decision.conclusion, Conclusion::Success, "{decision:?}");
    assert_ne!(decision.key, recorded.key, "a new decision has a new key");
    Ok(())
}

/// The pipeline's `admit` admitting a `micro` spend, then the placeholder review (D11).
fn pipeline_jobs() -> (AdmitJob, ReviewJob) {
    (
        AdmitJob {
            result: JobResult::Success,
            degrade: TierDegrade::None,
            admission: AdmitOutcome::Spend { tier: Tier::Micro },
        },
        ReviewJob {
            result: JobResult::Success,
            stage: ReviewStage::NotRun,
            cost: Aeu::from_micros(0),
            documents: Vec::new(),
        },
    )
}

/// Evidence whose inputs the pipeline's own `decision_inputs` composed over `world`'s snapshot.
fn composed_evidence(world: &World) -> Result<EvidenceBundle, Box<dyn Error>> {
    evidence(world, |run| {
        let (admit, review) = pipeline_jobs();
        run.inputs = decision_inputs(&InputsRequest {
            run: &run.authenticated,
            state: &run.github,
            readiness: Readiness::Ready,
            requeues: 0,
            prior: &PriorWaivers::default(),
            admit,
            review: &review,
        })?;
        Ok(())
    })
}

#[test]
fn a_re_decision_over_the_evidence_s_own_snapshot_composes_the_same_inputs_and_key()
-> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    world.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 5));
    let evidence = composed_evidence(&world)?;
    let state = redecided(&world, &evidence, &lists())?;

    let redecision = inputs(&evidence, &state, 0)?;

    assert_eq!(&redecision, evidence.inputs());
    Ok(())
}

#[test]
fn a_snapshot_of_another_head_is_refused() -> Result<(), Box<dyn Error>> {
    let world = contributor_pr();
    let evidence = composed_evidence(&world)?;
    let other = World::opened_at(CONTRIBUTOR, 'c').state()?;

    let outcome = inputs(&evidence, &other, 0);

    assert!(
        matches!(outcome, Err(InputsError::OtherHead)),
        "{outcome:?}"
    );
    Ok(())
}

/// `inputs` with every field a re-decision recomputes from the snapshot set to `like`'s.
fn but_snapshot(mut inputs: DecisionInputs, like: &DecisionInputs) -> DecisionInputs {
    inputs.decision_key.clone_from(&like.decision_key);
    inputs.pr.owner_approval = like.pr.owner_approval;
    inputs.pr.authorship = like.pr.authorship;
    inputs.pr.requeues = like.pr.requeues;
    inputs.waivers.clone_from(&like.waivers);
    inputs.override_record.clone_from(&like.override_record);
    inputs
}

/// A review by one of the PR's people: who, approving or not, at which minute.
fn review() -> impl Strategy<Value = (u64, bool, u32)> {
    (
        proptest::sample::select(vec![OWNER, APPROVER, CONTRIBUTOR, DAILY]),
        any::<bool>(),
        0..30_u32,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn only_the_snapshot_s_fields_differ_from_the_evidence_inputs(
        reviews in vec(review(), 0..4),
        requeues in 0..3_u32,
        trust_path in any::<bool>(),
    ) {
        let mut world = contributor_pr();
        let evidence = evidence(&world, |run| {
            if trust_path {
                run.inputs.pr.classes = vec![PrClass::TrustPath];
            }
            Ok(())
        }).expect("the evidence reads");
        for (reviewer, approves, minute) in reviews {
            let state = if approves { ReviewState::Approved } else { ReviewState::ChangesRequested };
            world.review(reviewer, state, (HEAD_COMMIT, minute));
        }
        let state = redecided(&world, &evidence, &lists()).expect("the head did not move");

        let redecision = inputs(&evidence, &state, requeues).expect("the inputs compose");

        prop_assert_eq!(redecision.pr.owner_approval, state.owner_approval());
        prop_assert_eq!(redecision.pr.requeues, requeues);
        prop_assert_eq!(&but_snapshot(redecision, evidence.inputs()), evidence.inputs());
    }
}

/// The head before the scenario's: the approver waived its `raised` finding there.
const WAIVED_HEAD: char = 'a';

/// The approver's waive of `raised` on [`WAIVED_HEAD`] at minute 5, then the contributor's push
/// of the scenario's head at minute 7; and the evidence of that head's run, whose decision
/// carried the waiver.
fn carried_waiver() -> Result<(World, EvidenceBundle), Box<dyn Error>> {
    let scenario = raised_scenario()?;
    let mut world = World::opened_at(CONTRIBUTOR, WAIVED_HEAD);
    let fp = {
        let run = crate::decide_fakes::authenticated(scenario.authenticate()?)?;
        run.gates()
            .first()
            .and_then(|gate| gate.findings.first())
            .ok_or("the static gate has a finding")?
            .fp
    };
    let posted = comment(40, APPROVER, 5, &format!("/assay waive {fp}"));
    let assay_app::Recorded::Record(record) =
        world.state()?.record_command(&created(posted.clone()))?
    else {
        return Err("a waive is recorded".into());
    };
    world.commands.push(record);
    world.comments.insert(posted.id, posted);
    world.commit(HEAD_COMMIT, &[WAIVED_HEAD], By::user(CONTRIBUTOR));
    world.push((WAIVED_HEAD, HEAD_COMMIT), CONTRIBUTOR, 7);
    world.head(HEAD_COMMIT);
    let evidence = evidence_of(&scenario, &world, |run| {
        run.inputs.waivers = by_approver(fp);
        Ok(())
    })?;
    assert_eq!(
        evidence.prior_waivers().fingerprints().collect::<Vec<_>>(),
        vec![fp],
        "the evidence decision waived the finding"
    );
    Ok((world, evidence))
}

#[test]
fn a_waiver_the_evidence_carried_survives_while_its_record_stands() -> Result<(), Box<dyn Error>> {
    let (world, evidence) = carried_waiver()?;
    let state = redecided(&world, &evidence, &lists())?;

    let redecision = inputs(&evidence, &state, 0)?;

    assert_eq!(redecision.waivers, evidence.inputs().waivers);
    assert_eq!(
        decide(&rubric_v1()?, &redecision).waived,
        evidence.decision().waived
    );
    Ok(())
}

#[test]
fn a_waiver_the_evidence_carried_drops_once_its_record_is_voided() -> Result<(), Box<dyn Error>> {
    let (mut world, evidence) = carried_waiver()?;
    world.comments.clear();
    let state = redecided(&world, &evidence, &lists())?;

    let redecision = inputs(&evidence, &state, 0)?;

    assert_eq!(redecision.waivers, Vec::new());
    assert_eq!(decide(&rubric_v1()?, &redecision).waived, Vec::new());
    Ok(())
}
