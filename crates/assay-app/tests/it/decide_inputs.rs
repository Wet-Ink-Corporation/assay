//! `decide`'s composition of the rubric's inputs (#60 ST-12): the authenticated run, the GitHub
//! state snapshot and the `admit` and `review` job outputs, with the decision key of 02 §5.

use std::error::Error;

use assay_app::{
    AdmitJob, AdmitOutcome, AuthenticatedRun, Deferred, GitHubState, InputsError, InputsRequest,
    JobResult, PriorWaivers, Recorded, ReviewJob, ReviewStage, ReviewState, decision_inputs,
};
use assay_domain::{
    Aeu, DecisionInputs, Deferral, InfraPolicy, LlmStage, NotRunReason, OwnerApproval, Readiness,
    Tier, WeighIn,
};

use crate::decide_fakes::{Scenario, authenticated};
use crate::eligibility_fakes::{APPROVER, CONTRIBUTOR, World, comment, created, fp};

/// The scenario's head, as the eligibility fakes name commits.
const HEAD: char = 'b';

type Outcome = Result<(), Box<dyn Error>>;

/// A successful `admit` that admitted `admission`.
const fn admitted(admission: AdmitOutcome) -> AdmitJob {
    AdmitJob {
        result: JobResult::Success,
        degrade: assay_app::TierDegrade::None,
        admission,
    }
}

/// The `review` placeholder's outputs (D11): `stage=not-run`, nothing spent, no documents.
const fn placeholder() -> ReviewJob {
    ReviewJob {
        result: JobResult::Success,
        stage: ReviewStage::NotRun,
        cost: Aeu::from_micros(0),
        documents: Vec::new(),
    }
}

/// A spend admission at `micro`, the scenario's tier.
const SPEND: AdmitOutcome = AdmitOutcome::Spend { tier: Tier::Micro };

/// What a decision on the scenario's run reads.
struct Fixture {
    run: AuthenticatedRun,
    world: World,
    readiness: Readiness,
    requeues: u32,
    admit: AdmitJob,
    review: ReviewJob,
}

impl Fixture {
    /// The default scenario's `micro` run, a contributor's ready PR nobody approved, admitted
    /// to spend, with the placeholder review.
    fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            run: authenticated(Scenario::new()?.authenticate()?)?,
            world: World::opened_at(CONTRIBUTOR, HEAD),
            readiness: Readiness::Ready,
            requeues: 0,
            admit: admitted(SPEND),
            review: placeholder(),
        })
    }

    fn inputs(&self) -> Result<DecisionInputs, Box<dyn Error>> {
        let state = self.world.state()?;
        Ok(self.inputs_over(&state)?)
    }

    fn inputs_over(&self, state: &GitHubState) -> Result<DecisionInputs, InputsError> {
        decision_inputs(&InputsRequest {
            run: &self.run,
            state,
            readiness: self.readiness,
            requeues: self.requeues,
            prior: &PriorWaivers::default(),
            admit: self.admit,
            review: &self.review,
        })
    }

    /// Records `body` as the approver's comment `id` on the head.
    fn command(&mut self, id: u64, body: &str) -> Outcome {
        let posted = comment(id, APPROVER, 5, body);
        let Recorded::Record(record) = self
            .world
            .state()?
            .record_command(&created(posted.clone()))?
        else {
            return Err("a privileged command is recorded".into());
        };
        self.world.commands.push(record);
        self.world.comments.insert(posted.id, posted);
        Ok(())
    }
}

const INFRA: LlmStage = LlmStage::Deferred {
    reason: Deferral::InfraLlmUnavailable,
};

#[test]
fn the_placeholder_review_of_a_spent_run_reads_as_an_unavailable_llm() -> Outcome {
    let fixture = Fixture::new()?;

    let inputs = fixture.inputs()?;

    assert_eq!(inputs.pr.tier, Tier::Micro);
    assert_eq!(inputs.llm, INFRA);
    assert!(inputs.findings.is_empty());
    assert!(inputs.completed_roles.is_empty());
    Ok(())
}

#[test]
fn a_deferred_admission_is_deferred_with_its_own_code() -> Outcome {
    for (deferred, reason) in [
        (Deferred::Budget, Deferral::BudgetDeferred),
        (Deferred::SlotBusy, Deferral::SlotBusy),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.admit = admitted(AdmitOutcome::Deferred(deferred));

        assert_eq!(fixture.inputs()?.llm, LlmStage::Deferred { reason });
    }
    Ok(())
}

#[test]
fn a_draft_admission_stays_not_run_while_the_snapshot_says_draft() -> Outcome {
    let mut fixture = Fixture::new()?;
    fixture.admit = admitted(AdmitOutcome::NoSpend(NotRunReason::Draft));
    fixture.readiness = Readiness::Draft;

    let inputs = fixture.inputs()?;

    assert_eq!(
        inputs.llm,
        LlmStage::NotRun {
            reason: NotRunReason::Draft
        }
    );
    assert_eq!(inputs.pr.readiness, Readiness::Draft);
    Ok(())
}

#[test]
fn a_draft_admission_of_a_pr_marked_ready_since_reads_as_an_unavailable_llm() -> Outcome {
    let mut fixture = Fixture::new()?;
    fixture.admit = admitted(AdmitOutcome::NoSpend(NotRunReason::Draft));

    assert_eq!(fixture.inputs()?.llm, INFRA);
    Ok(())
}

#[test]
fn a_reused_admission_is_refused_as_an_unavailable_llm_until_reuse_is_built() -> Outcome {
    let mut fixture = Fixture::new()?;
    fixture.admit = admitted(AdmitOutcome::Reused);

    assert_eq!(fixture.inputs()?.llm, INFRA);
    Ok(())
}

#[test]
fn an_admit_that_did_not_succeed_reads_as_an_unavailable_llm_whatever_it_output() -> Outcome {
    for result in [JobResult::Failure, JobResult::Skipped, JobResult::Cancelled] {
        let mut fixture = Fixture::new()?;
        fixture.admit = AdmitJob {
            result,
            ..admitted(AdmitOutcome::NoSpend(NotRunReason::Draft))
        };
        fixture.readiness = Readiness::Draft;

        assert_eq!(fixture.inputs()?.llm, INFRA, "{result:?}");
    }
    Ok(())
}

#[test]
fn a_snapshot_of_another_head_is_refused() -> Outcome {
    let fixture = Fixture::new()?;
    let elsewhere = World::opened_at(CONTRIBUTOR, 'c').state()?;

    let refused = fixture.inputs_over(&elsewhere);

    assert!(
        matches!(refused, Err(InputsError::OtherHead)),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn the_inputs_carry_the_run_and_the_snapshot() -> Outcome {
    let mut fixture = Fixture::new()?;
    fixture.requeues = 2;
    fixture.command(31, &format!("/assay waive {}", fp()))?;

    let inputs = fixture.inputs()?;

    assert_eq!(inputs.evidence_key, fixture.run.run_key().to_string());
    assert_eq!(inputs.policy, *fixture.run.policy());
    assert_eq!(inputs.gates, fixture.run.gates());
    assert_eq!(inputs.pr.base_ref, "main");
    assert_eq!(inputs.pr.open_prs_on_head, 1);
    assert_eq!(inputs.pr.owner_approval, OwnerApproval::NotApproved);
    assert_eq!(inputs.pr.requeues, 2);
    assert_eq!(inputs.weigh_in, WeighIn::NotRequired);
    assert_eq!(inputs.waivers.len(), 1);
    assert_eq!(inputs.waivers.first().map(|waiver| waiver.fp), Some(fp()));
    assert_eq!(inputs.override_record, None);
    Ok(())
}

/// The decision key of the default fixture, pinned: changing what it hashes, or how, changes
/// every published check's `external_id`, so a re-decision never takes an old check for its
/// own. (The owner-approved corpus case under `corpus/golden/` is left for the owner, T16.)
///
/// The expected key was computed independently, with Python's `hashlib`, from the pinned
/// evidence key and policy and these parts, each name and value prefixed by its byte length as a
/// big-endian `u64`: `approvals_digest`, `waivers_digest` and `dismissals_digest` each the
/// sha256 of `[]`, `override_state` `null`, `on_infra_failure_policy` `warn`, and
/// `deferral_state`
/// `{"llm":{"reason":"infra-llm-unavailable","status":"deferred"},"readiness":"ready","requeues":0}`.
#[test]
fn the_decision_key_of_the_default_run_is_pinned() -> Outcome {
    let inputs = Fixture::new()?.inputs()?;

    assert_eq!(
        (inputs.evidence_key.as_str(), inputs.policy.on_infra_failure),
        (
            "sha256:ed30d0fb61bccd95c8a99a7386a7112c4e386c7c4f5576a336efcce56655b35f",
            InfraPolicy::Warn
        )
    );
    assert_eq!(
        inputs.decision_key,
        "sha256:d1b4a98ff327b53792e3f0e7c824eeac9c5700731cec9578985773e9870eae81"
    );
    Ok(())
}

#[test]
fn the_decision_key_changes_with_each_snapshot_fact_it_reads() -> Outcome {
    let base = Fixture::new()?.inputs()?.decision_key;

    let mut approved = Fixture::new()?;
    approved
        .world
        .review(APPROVER, ReviewState::Approved, (HEAD, 5));
    let mut waived = Fixture::new()?;
    waived.command(31, &format!("/assay waive {}", fp()))?;
    let mut dismissed = Fixture::new()?;
    dismissed.command(32, &format!("/assay dismiss {} not a defect", fp()))?;
    let mut draft = Fixture::new()?;
    draft.readiness = Readiness::Draft;
    let mut requeued = Fixture::new()?;
    requeued.requeues = 1;
    let mut deferred = Fixture::new()?;
    deferred.admit = admitted(AdmitOutcome::Deferred(Deferred::SlotBusy));

    let mut keys = vec![base];
    for fixture in [approved, waived, dismissed, draft, requeued, deferred] {
        keys.push(fixture.inputs()?.decision_key);
    }

    let distinct: std::collections::BTreeSet<&String> = keys.iter().collect();
    assert_eq!(distinct.len(), keys.len(), "{keys:#?}");
    Ok(())
}

#[test]
fn the_decision_key_is_a_sha256_digest_and_stable_across_builds() -> Outcome {
    let fixture = Fixture::new()?;

    let first = fixture.inputs()?.decision_key;

    assert!(first.parse::<assay_app::Sha256Digest>().is_ok(), "{first}");
    assert_eq!(fixture.inputs()?.decision_key, first);
    Ok(())
}
