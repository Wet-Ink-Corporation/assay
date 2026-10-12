//! An authenticated run from the `decide` scenario, ready to publish over [`FakeWorld`].

use std::error::Error;

use assay_app::{
    AuthenticatedRun, DecideRequest, Decided, Publication, PublishError, PublishPorts, Published,
    Snapshot, Target, Verdict, Written, publish,
};
use assay_domain::{
    Aeu, Authorship, DecisionInputs, Event, LlmStage, OwnerApproval, Readiness, RequiredRole,
    Rubric, SupportOutcome, WeighIn,
};

use crate::decide_fakes::{Scenario, authenticated};
use crate::plan_fakes::{FakeError, HEAD};
use crate::publish_fakes::FakeWorld;
use crate::support::rubric_v1;

/// The decision key the inputs carry.
pub(crate) const KEY: &str =
    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
/// `open`'s stamp for run 7 (`plan_fakes::run_uid`).
pub(crate) const STAMP: &str = "pending:7:3";
/// The verdict check's name on the default branch.
pub(crate) const NAME: &str = "assay / verdict";
/// The repository's default branch.
pub(crate) const DEFAULT_BRANCH: &str = "main";

/// What `publish` returns over the fake.
pub(crate) type Outcome =
    Result<Published<FakeError, FakeError>, PublishError<FakeError, FakeError>>;

/// One authenticated run and its decision inputs.
#[derive(Debug)]
pub(crate) struct Run {
    pub(crate) request: DecideRequest,
    pub(crate) authenticated: AuthenticatedRun,
    pub(crate) inputs: DecisionInputs,
    pub(crate) rubric: Rubric,
    pub(crate) events: Vec<Event>,
}

impl Run {
    /// The default scenario's run: a `micro` PR whose stage completed, deciding `success`.
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        Self::of(&Scenario::new()?)
    }

    /// `scenario`'s run, its LLM stage completed by `combined-micro`.
    pub(crate) fn of(scenario: &Scenario) -> Result<Self, Box<dyn Error>> {
        let run = authenticated(scenario.authenticate()?)?;
        let inputs = inputs(&run);
        Ok(Self {
            request: scenario.request.clone(),
            authenticated: run,
            inputs,
            rubric: rubric_v1()?,
            events: Vec::new(),
        })
    }

    /// The run's target on a repository whose default branch is `main`.
    pub(crate) fn target(&self) -> Target<'_> {
        Target {
            run_uid: self.request.plan.run_uid,
            pull_request: &self.request.plan.pull_request,
            default_branch: DEFAULT_BRANCH,
        }
    }

    /// Publishes the run's decision over `world`.
    pub(crate) fn publish(&self, world: &FakeWorld) -> Outcome {
        let publication = Publication {
            target: self.target(),
            verdict: Verdict::Decided(Decided {
                run: &self.authenticated,
                rubric: &self.rubric,
                inputs: &self.inputs,
                events: &self.events,
                cost: Aeu::from_micros(740_000),
            }),
        };
        publish(&publication, &ports(world))
    }
}

/// `world` as all three ports.
pub(crate) const fn ports(world: &FakeWorld) -> PublishPorts<'_, FakeWorld, FakeWorld, FakeWorld> {
    PublishPorts {
        forge: world,
        ledger: world,
        bundles: world,
    }
}

/// A world holding the App check on the scenario's head, stamped by `open` for run 7.
pub(crate) fn stamped() -> Result<FakeWorld, Box<dyn Error>> {
    FakeWorld::with_check(HEAD, NAME, STAMP)
}

/// What was written, or why nothing was.
pub(crate) fn written(outcome: Outcome) -> Result<Written<FakeError, FakeError>, Box<dyn Error>> {
    match outcome? {
        Published::Written(written) => Ok(written),
        other => Err(format!("expected a write, got {other:?}").into()),
    }
}

/// The decision inputs of `run`: a ready PR by someone else, not approved, whose `micro` stage
/// completed with `combined-micro`.
fn inputs(run: &AuthenticatedRun) -> DecisionInputs {
    DecisionInputs {
        decision_key: KEY.to_owned(),
        evidence_key: run.run_key().to_string(),
        policy: run.policy().clone(),
        pr: run.pull_request(Snapshot {
            owner_approval: OwnerApproval::NotApproved,
            authorship: Authorship::Other,
            readiness: Readiness::Ready,
            requeues: 0,
        }),
        gates: run.gates().to_vec(),
        weigh_in: WeighIn::NotRequired,
        llm: LlmStage::Completed,
        findings: Vec::new(),
        dispositions: Vec::new(),
        completed_roles: [RequiredRole::CombinedMicro].into_iter().collect(),
        coordinator: SupportOutcome::NotNeeded,
        verifier: SupportOutcome::NotNeeded,
        degraded: Vec::new(),
        waivers: Vec::new(),
        override_record: None,
    }
}
