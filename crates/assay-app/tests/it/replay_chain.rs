//! Replay follows the chain (#61 DO-8, J10, R-09): a decide-only decision bundle replays through
//! the evidence bundle it names. The decision run must be `assay-decide.yml`'s, the inputs must
//! keep every field of the evidence's a snapshot does not decide, and the recorded decision
//! must recompute byte for byte.

use std::assert_matches;
use std::cell::RefCell;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    Absence, BUNDLE_FILE, BundleShape, BundleSource, ChainError, ChainMismatch, ChainPorts,
    ChainReplayed, ChainRequest, DecisionBundleSource, DecisionPointer, DecisionTarget,
    DecisionTrigger, Forgery, InputsField, MAX_BUNDLE_BYTES, MAX_DECISION_BUNDLE_BYTES,
    ProvenanceRefusal, Recorded, Redecided, RedecisionRequest, ReviewState, RunEvent, RunField,
    RunProvenance, Sha256Digest, StoredBundle, WorkflowRunFacts, prepare_decision,
    redecision_inputs, replay_chain,
};
use assay_domain::{DecisionInputs, LlmStage, NotRunReason, PrClass, Rubric, RunUid, Tier, decide};
use serde_json::{Value, json};

use crate::decide_fakes::{Scenario, authenticated};
use crate::eligibility_fakes::{
    APPROVER, CONTRIBUTOR, DIGEST, World, comment, created, lists, sha,
};
use crate::plan_fakes::FakeError;
use crate::publish_support::{HEAD_COMMIT, Run};
use crate::redecide_fakes::{StoredEvidence, redecided, stored_evidence};
use crate::replay_fakes::Stored;
use crate::support::rubric_v1;
use crate::waive_carry::raised_scenario;

/// The repository of every run here (`plan_fakes::run_uid`).
const REPO: u64 = 840_213_377;

/// The decide-only run that re-decided.
const DECIDE_ONLY_RUN: u64 = 99;

/// The PR whose decision bundle is replayed.
const PR: u64 = 7;

fn id(value: u64) -> Result<NonZeroU64, Box<dyn Error>> {
    Ok(NonZeroU64::new(value).ok_or("not zero")?)
}

/// A port call, in the order the chain made it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    /// A run's provenance was read.
    Provenance(u64),
    /// The decision bundle of a run and PR was fetched, under a limit.
    Decision(RunUid, u64, usize),
    /// The evidence bundle's file of a run was fetched, under a limit.
    Evidence(RunUid, String, usize),
}

/// GitHub's runs and the artifact store, logging every call into one list.
struct Ports {
    runs: Vec<WorkflowRunFacts>,
    /// The run whose provenance GitHub fails to answer for, if any.
    unreadable: Option<NonZeroU64>,
    evidence: Stored,
    decision: Stored,
    calls: RefCell<Vec<Call>>,
}

/// What the store answers for `stored`.
fn answer(stored: &Stored) -> Result<StoredBundle, FakeError> {
    match stored {
        Stored::Bytes(bytes) => Ok(StoredBundle::Found(bytes.clone())),
        Stored::Missing => Ok(StoredBundle::Missing),
        Stored::Expired => Ok(StoredBundle::Expired),
        Stored::TooLarge => Ok(StoredBundle::TooLarge),
        Stored::Fails => Err(FakeError),
    }
}

impl RunProvenance for Ports {
    type Error = FakeError;

    fn workflow_run(&self, run_id: NonZeroU64) -> Result<Option<WorkflowRunFacts>, FakeError> {
        self.calls.borrow_mut().push(Call::Provenance(run_id.get()));
        if self.unreadable == Some(run_id) {
            return Err(FakeError);
        }
        Ok(self.runs.iter().find(|run| run.run_id == run_id).cloned())
    }
}

impl BundleSource for Ports {
    type Error = FakeError;

    fn fetch(&self, run: RunUid, file: &str, limit: usize) -> Result<StoredBundle, FakeError> {
        self.calls
            .borrow_mut()
            .push(Call::Evidence(run, file.to_owned(), limit));
        answer(&self.evidence)
    }
}

impl DecisionBundleSource for Ports {
    type Error = FakeError;

    fn fetch_decision(
        &self,
        run: RunUid,
        pr: NonZeroU64,
        limit: usize,
    ) -> Result<StoredBundle, FakeError> {
        self.calls
            .borrow_mut()
            .push(Call::Decision(run, pr.get(), limit));
        answer(&self.decision)
    }
}

/// What `replay_chain` returns over the fakes.
type Outcome = Result<ChainReplayed, ChainError<FakeError, FakeError, FakeError>>;

/// A decide-only run's facts changed one way, and the refusal that change is.
type RunCase = (&'static str, fn(&mut WorkflowRunFacts), ProvenanceRefusal);

/// A scheduled re-decision of a contributor's PR, persisted, with the evidence it names.
struct Chain {
    evidence: StoredEvidence,
    inputs: DecisionInputs,
    /// The decision bundle's bytes, as uploaded.
    decision: Vec<u8>,
    pointer: DecisionPointer,
    decide_only: WorkflowRunFacts,
    rubric: Rubric,
}

/// What a chain's evidence is taken over, and what happens on GitHub after it.
struct Setup<'a, A, B> {
    /// The pipeline run whose evidence is taken.
    scenario: &'a Scenario,
    /// The PR as the evidence run saw it.
    world: World,
    /// Applied to the run before it is published.
    evidence_edit: A,
    /// What happens on GitHub after the evidence, before decide-only re-decides.
    later: B,
}

impl Chain {
    /// The re-decision decide-only run `run` persisted, its inputs changed by `edit` first.
    fn of(run: u64, edit: impl FnOnce(&mut DecisionInputs)) -> Result<Self, Box<dyn Error>> {
        Self::built(
            run,
            Setup {
                scenario: &Scenario::new()?,
                world: World::opened_at(CONTRIBUTOR, HEAD_COMMIT),
                evidence_edit: |_: &mut Run| Ok(()),
                later: |_: &mut World| Ok(()),
            },
            edit,
        )
    }

    /// The re-decision run `run` persisted of `setup`'s world, the inputs decide-only composes
    /// changed by `edit` before the bundle is written.
    fn built<A, B>(
        run: u64,
        setup: Setup<'_, A, B>,
        edit: impl FnOnce(&mut DecisionInputs),
    ) -> Result<Self, Box<dyn Error>>
    where
        A: FnOnce(&mut Run) -> Result<(), Box<dyn Error>>,
        B: FnOnce(&mut World) -> Result<(), Box<dyn Error>>,
    {
        let Setup {
            scenario,
            mut world,
            evidence_edit,
            later,
        } = setup;
        let evidence = stored_evidence(scenario, &world, evidence_edit)?;
        later(&mut world)?;
        let state = redecided(&world, &evidence.bundle, &lists())?;
        let mut inputs = redecision_inputs(&RedecisionRequest {
            evidence: &evidence.bundle,
            state: &state,
            requeues: 0,
        })?;
        edit(&mut inputs);
        let rubric = rubric_v1()?;
        let run_uid = RunUid {
            repo_id: id(REPO)?,
            run_id: id(run)?,
            run_attempt: NonZeroU64::MIN,
        };
        let prepared = prepare_decision(
            &DecisionTarget {
                run_uid,
                head_sha: evidence.bundle.head(),
                base_ref: evidence.bundle.base_ref(),
                trigger: DecisionTrigger::Schedule,
                at: "2026-11-01T10:00:00Z".parse()?,
            },
            &Redecided {
                evidence: &evidence.bundle,
                rubric: &rubric,
                inputs: &inputs,
                github: &state,
                command: None,
            },
        )?;
        let pointer = DecisionPointer {
            run_id: run_uid.run_id,
            sha256: prepared.digest().clone(),
        };
        let decision = prepared.bytes().to_vec();
        let decide_only = WorkflowRunFacts {
            run_id: run_uid.run_id,
            repo_id: run_uid.repo_id,
            path: ".github/workflows/assay-decide.yml".to_owned(),
            event: RunEvent::Schedule,
            head_branch: "main".to_owned(),
            head_repo: Some(run_uid.repo_id),
            run_attempt: NonZeroU64::MIN,
        };
        Ok(Self {
            evidence,
            inputs,
            decision,
            pointer,
            decide_only,
            rubric,
        })
    }

    /// decide-only run 99's re-decision, its inputs as `redecision_inputs` composed them.
    fn plain() -> Result<Self, Box<dyn Error>> {
        Self::of(DECIDE_ONLY_RUN, |_| ())
    }

    /// The plain chain with its decision bundle rewritten by `edit` and the pointer moved to
    /// the new bytes: a bundle the strict reader still reads, but not one assay wrote.
    fn rewritten(edit: impl FnOnce(&mut Value)) -> Result<Self, Box<dyn Error>> {
        let mut chain = Self::plain()?;
        let mut value: Value = serde_json::from_slice(&chain.decision)?;
        edit(&mut value);
        chain.decision = serde_json::to_vec(&value)?;
        chain.pointer.sha256 = Sha256Digest::of(&chain.decision);
        Ok(chain)
    }

    /// GitHub knowing both runs, and the store holding both bundles.
    fn ports(&self) -> Ports {
        Ports {
            runs: vec![self.evidence.run.clone(), self.decide_only.clone()],
            unreadable: None,
            evidence: Stored::Bytes(self.evidence.bytes.clone()),
            decision: Stored::Bytes(self.decision.clone()),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Replays the chain from `self.pointer` under `rubric`, over `ports`.
    fn replay_under(&self, ports: &Ports, rubric: &Rubric) -> Result<Outcome, Box<dyn Error>> {
        Ok(replay_chain(
            &ChainRequest {
                repo_id: id(REPO)?,
                pr: id(PR)?,
                pointer: &self.pointer,
                default_branch: "main",
                rubric,
            },
            &ChainPorts {
                provenance: ports,
                evidence: ports,
                decisions: ports,
            },
        ))
    }

    /// Replays the chain under rubric v1.
    fn replay(&self, ports: &Ports) -> Result<Outcome, Box<dyn Error>> {
        self.replay_under(ports, &self.rubric)
    }

    /// The mismatch the chain replays to over its own ports.
    fn mismatch(&self) -> Result<ChainMismatch, Box<dyn Error>> {
        match self.replay(&self.ports())?? {
            ChainReplayed::Mismatch(mismatch) => Ok(mismatch),
            ChainReplayed::Match(_) => Err("a mismatch, got a match".into()),
        }
    }
}

#[test]
fn a_decision_bundle_replays_to_its_recorded_decision_through_its_evidence()
-> Result<(), Box<dyn Error>> {
    let chain = Chain::plain()?;
    let ports = chain.ports();

    let ChainReplayed::Match(matched) = chain.replay(&ports)?? else {
        return Err("the chain replays".into());
    };

    assert_eq!(matched.decision, decide(&chain.rubric, &chain.inputs));
    assert_eq!(matched.evidence, chain.evidence.bundle);
    let decision_run = RunUid {
        repo_id: id(REPO)?,
        run_id: id(DECIDE_ONLY_RUN)?,
        run_attempt: NonZeroU64::MIN,
    };
    let evidence_run = RunUid {
        repo_id: id(REPO)?,
        run_id: chain.evidence.pointer.run_id,
        run_attempt: NonZeroU64::MIN,
    };
    assert_eq!(
        ports.calls.into_inner(),
        [
            Call::Provenance(DECIDE_ONLY_RUN),
            Call::Decision(decision_run, PR, MAX_DECISION_BUNDLE_BYTES),
            Call::Provenance(chain.evidence.pointer.run_id.get()),
            Call::Evidence(evidence_run, BUNDLE_FILE.to_owned(), MAX_BUNDLE_BYTES),
        ],
        "the decision run's provenance first, then its bundle, then the evidence's"
    );
    Ok(())
}

#[test]
fn inputs_that_differ_from_the_evidence_on_a_gate_or_the_llm_stage_are_a_mismatch()
-> Result<(), Box<dyn Error>> {
    let gate = Chain::of(DECIDE_ONLY_RUN, |inputs| {
        if let Some(first) = inputs.gates.first_mut() {
            first.required = !first.required;
        }
    })?;
    assert!(
        !gate.inputs.gates.is_empty(),
        "the scenario has a gate to change"
    );
    let llm = Chain::of(DECIDE_ONLY_RUN, |inputs| {
        inputs.llm = if inputs.llm == LlmStage::Completed {
            LlmStage::NotRun {
                reason: NotRunReason::NoLlmTier,
            }
        } else {
            LlmStage::Completed
        };
    })?;

    assert_eq!(gate.mismatch()?, ChainMismatch::Inputs(InputsField::Gates));
    assert_eq!(llm.mismatch()?, ChainMismatch::Inputs(InputsField::Llm));
    Ok(())
}

#[test]
fn a_decision_bundle_of_a_run_other_than_assay_decide_yml_is_refused_before_any_fetch()
-> Result<(), Box<dyn Error>> {
    let chain = Chain::plain()?;
    let cases: [RunCase; 4] = [
        (
            "the pipeline's workflow",
            |run| ".github/workflows/assay.yml".clone_into(&mut run.path),
            ProvenanceRefusal::Workflow,
        ),
        (
            "a pull_request run",
            |run| run.event = RunEvent::Other,
            ProvenanceRefusal::Event,
        ),
        (
            "a branch run",
            |run| "feature".clone_into(&mut run.head_branch),
            ProvenanceRefusal::Branch,
        ),
        (
            "a re-run",
            |run| run.run_attempt = NonZeroU64::MIN.saturating_add(1),
            ProvenanceRefusal::Attempt,
        ),
    ];
    for (case, edit, refusal) in cases {
        let mut ports = chain.ports();
        for run in &mut ports.runs {
            if run.run_id.get() == DECIDE_ONLY_RUN {
                edit(run);
            }
        }

        let outcome = chain.replay(&ports)??;

        assert_eq!(
            outcome,
            ChainReplayed::Mismatch(ChainMismatch::Provenance(refusal)),
            "{case}"
        );
        assert_eq!(
            ports.calls.into_inner(),
            [Call::Provenance(DECIDE_ONLY_RUN)],
            "{case}: nothing fetched"
        );
    }
    let mut unknown = chain.ports();
    unknown
        .runs
        .retain(|run| run.run_id.get() != DECIDE_ONLY_RUN);
    assert_eq!(
        chain.replay(&unknown)??,
        ChainReplayed::Mismatch(ChainMismatch::Provenance(ProvenanceRefusal::Missing))
    );
    Ok(())
}

#[test]
fn bytes_that_do_not_hash_to_the_pointer_or_name_another_run_are_a_mismatch()
-> Result<(), Box<dyn Error>> {
    let mut tampered = Chain::plain()?;
    let other = Sha256Digest::of(b"another bundle");
    tampered.pointer.sha256 = other.clone();

    assert_eq!(
        tampered.mismatch()?,
        ChainMismatch::Digest {
            pointer: other,
            bundle: Sha256Digest::of(&tampered.decision),
        }
    );

    let mut moved = Chain::of(DECIDE_ONLY_RUN.saturating_add(1), |_| ())?;
    moved.pointer.run_id = id(DECIDE_ONLY_RUN)?;
    moved.decide_only.run_id = id(DECIDE_ONLY_RUN)?;

    assert_eq!(moved.mismatch()?, ChainMismatch::Run(RunField::RunId));
    Ok(())
}

#[test]
fn a_recorded_decision_its_inputs_do_not_give_is_a_mismatch() -> Result<(), Box<dyn Error>> {
    let chain = Chain::rewritten(|bundle| {
        bundle["decision"]["key"] = Value::from(DIGEST);
    })?;

    assert_matches!(chain.mismatch()?, ChainMismatch::Decision(_));
    Ok(())
}

#[test]
fn a_decision_bundle_of_another_head_than_its_evidence_is_a_mismatch() -> Result<(), Box<dyn Error>>
{
    let chain = Chain::rewritten(|bundle| {
        bundle["github"]["head_sha"] = Value::from(sha('e').as_str());
    })?;

    assert_eq!(chain.mismatch()?, ChainMismatch::Head);
    Ok(())
}

#[test]
fn evidence_decided_under_another_rubric_than_its_decision_is_a_mismatch()
-> Result<(), Box<dyn Error>> {
    let chain = Chain::rewritten(|bundle| {
        bundle["rubric_version"] = Value::from("2");
        bundle["decision"]["rubric_version"] = Value::from("2");
    })?;
    let rubric = other_rubric()?;

    assert_eq!(
        chain.replay_under(&chain.ports(), &rubric)??,
        ChainReplayed::Mismatch(ChainMismatch::EvidenceRubric)
    );
    Ok(())
}

/// A rubric of another version than the bundles'.
fn other_rubric() -> Result<Rubric, Box<dyn Error>> {
    Ok(toml::from_str(
        "version = \"2\"\nmajors_block_threshold = 3\nmax_requeues = 3\nmicro_roles = []\n",
    )?)
}

#[test]
fn forged_evidence_is_a_mismatch_and_absent_evidence_an_error() -> Result<(), Box<dyn Error>> {
    let chain = Chain::plain()?;
    let mut forged = chain.ports();
    for run in &mut forged.runs {
        if run.run_id == chain.evidence.pointer.run_id {
            ".github/workflows/assay-decide.yml".clone_into(&mut run.path);
        }
    }
    let mut missing = chain.ports();
    missing.evidence = Stored::Missing;
    let mut expired = chain.ports();
    expired.evidence = Stored::Expired;

    assert_eq!(
        chain.replay(&forged)??,
        ChainReplayed::Mismatch(ChainMismatch::Evidence(Forgery::Provenance(
            ProvenanceRefusal::Workflow
        )))
    );
    assert_matches!(
        chain.replay(&missing)?,
        Err(ChainError::EvidenceAbsent(Absence::Missing))
    );
    assert_matches!(
        chain.replay(&expired)?,
        Err(ChainError::EvidenceAbsent(Absence::Expired))
    );
    Ok(())
}

#[test]
fn a_decision_bundle_that_cannot_be_read_is_an_error() -> Result<(), Box<dyn Error>> {
    let chain = Chain::plain()?;
    let mut missing = chain.ports();
    missing.decision = Stored::Missing;
    let mut expired = chain.ports();
    expired.decision = Stored::Expired;
    let mut too_large = chain.ports();
    too_large.decision = Stored::TooLarge;
    let mut oversized = chain.ports();
    oversized.decision = Stored::Bytes(vec![b' '; MAX_DECISION_BUNDLE_BYTES.saturating_add(1)]);
    let mut fails = chain.ports();
    fails.decision = Stored::Fails;
    let rubric = other_rubric()?;
    let other_rubric = chain.ports();

    assert_matches!(chain.replay(&missing)?, Err(ChainError::Missing));
    assert_eq!(
        missing.calls.into_inner().len(),
        2,
        "no evidence is read for a missing decision bundle"
    );
    assert_matches!(chain.replay(&expired)?, Err(ChainError::Expired));
    assert_matches!(chain.replay(&too_large)?, Err(ChainError::TooLarge));
    assert_matches!(
        chain.replay(&oversized)?,
        Err(ChainError::TooLarge),
        "an adapter that reads past the cap is held to it"
    );
    assert_matches!(
        chain.replay(&fails)?,
        Err(ChainError::DecisionFetch(FakeError))
    );
    assert_matches!(
        chain.replay_under(&other_rubric, &rubric)?,
        Err(ChainError::RubricVersion)
    );
    Ok(())
}

#[test]
fn a_snapshot_that_changed_since_the_evidence_replays_as_recorded() -> Result<(), Box<dyn Error>> {
    let scenario = raised_scenario()?;
    let fp = authenticated(scenario.authenticate()?)?
        .gates()
        .first()
        .and_then(|gate| gate.findings.first())
        .ok_or("the static gate has a finding")?
        .fp;
    let setup = Setup {
        scenario: &scenario,
        world: World::opened_at(CONTRIBUTOR, HEAD_COMMIT),
        evidence_edit: |run: &mut Run| {
            run.inputs.pr.classes = vec![PrClass::TrustPath];
            Ok(())
        },
        later: |world: &mut World| -> Result<(), Box<dyn Error>> {
            world.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 5));
            let posted = comment(40, APPROVER, 6, &format!("/assay waive {fp}"));
            let Recorded::Record(record) =
                world.state()?.record_command(&created(posted.clone()))?
            else {
                return Err("the approver's waive is recorded".into());
            };
            world.commands.push(record);
            world.comments.insert(posted.id, posted);
            Ok(())
        },
    };
    let chain = Chain::built(DECIDE_ONLY_RUN, setup, |_| ())?;
    let evidence = &chain.evidence.bundle;
    let redecision = decide(&chain.rubric, &chain.inputs);
    assert_ne!(
        chain.inputs.pr.owner_approval,
        evidence.inputs().pr.owner_approval,
        "the approval came after the evidence"
    );
    assert_ne!(
        chain.inputs.waivers,
        evidence.inputs().waivers,
        "the waive came after the evidence"
    );
    assert_ne!(&redecision, evidence.decision());

    let ChainReplayed::Match(matched) = chain.replay(&chain.ports())?? else {
        return Err("a re-decision over a changed snapshot replays".into());
    };

    assert_eq!(matched.decision, redecision);
    Ok(())
}

#[test]
fn a_decision_bundle_off_the_contract_is_unreadable() -> Result<(), Box<dyn Error>> {
    let chain = Chain::rewritten(|bundle| {
        bundle["trigger"] = json!({"event": "push"});
    })?;

    assert_matches!(
        chain.replay(&chain.ports())?,
        Err(ChainError::Unreadable(BundleShape::Shape))
    );
    Ok(())
}

#[test]
fn a_port_that_cannot_be_read_is_its_error() -> Result<(), Box<dyn Error>> {
    let chain = Chain::plain()?;
    let mut decision_run = chain.ports();
    decision_run.unreadable = Some(id(DECIDE_ONLY_RUN)?);
    let mut evidence_run = chain.ports();
    evidence_run.unreadable = Some(chain.evidence.pointer.run_id);
    let mut evidence_store = chain.ports();
    evidence_store.evidence = Stored::Fails;

    assert_matches!(
        chain.replay(&decision_run)?,
        Err(ChainError::Provenance(FakeError))
    );
    assert_eq!(
        decision_run.calls.into_inner(),
        [Call::Provenance(DECIDE_ONLY_RUN)]
    );
    assert_matches!(
        chain.replay(&evidence_run)?,
        Err(ChainError::Provenance(FakeError))
    );
    assert_eq!(
        evidence_run.calls.into_inner().len(),
        3,
        "the evidence run's provenance failed after the decision bundle was read"
    );
    assert_matches!(
        chain.replay(&evidence_store)?,
        Err(ChainError::EvidenceFetch(FakeError))
    );
    Ok(())
}

#[test]
fn inputs_that_differ_from_the_evidence_on_the_tier_or_base_ref_are_a_mismatch()
-> Result<(), Box<dyn Error>> {
    let tier = Chain::rewritten(|bundle| {
        bundle["inputs"]["pr"]["tier"] = json!("needs-split");
    })?;
    assert_ne!(tier.inputs.pr.tier, Tier::NeedsSplit, "the tier changes");
    let base_ref = Chain::rewritten(|bundle| {
        bundle["inputs"]["pr"]["base_ref"] = json!("release");
    })?;
    assert_ne!(
        base_ref.inputs.pr.base_ref, "release",
        "the base ref changes"
    );

    assert_eq!(tier.mismatch()?, ChainMismatch::Inputs(InputsField::Tier));
    assert_eq!(
        base_ref.mismatch()?,
        ChainMismatch::Inputs(InputsField::BaseRef)
    );
    Ok(())
}
