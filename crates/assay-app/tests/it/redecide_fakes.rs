//! An authenticated evidence bundle of a [`World`]'s head, and the re-decision over it: what
//! decide-only reads once the evidence read believed the bundle (DO-5).

use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    EligibilityLists, EvidenceBundle, EvidencePointer, EvidenceRead, EvidenceRequest, GitHubState,
    RedecidePorts, RedecideRequest, RedecideStateError, RunEvent, RunProvenance, Sha256Digest,
    WorkflowRunFacts, read_evidence, redecided_state,
};

use crate::decide_fakes::Scenario;
use crate::eligibility_fakes::{FakeError, LISTS_AGENTS, World};
use crate::publish_support::{Run, stamped};
use crate::replay_fakes::{FakeBundles, Stored};
use crate::support::rubric_v1;

/// GitHub's answer for the pipeline's own `pull_request_target` run of `run`.
struct PipelineRun(WorkflowRunFacts);

impl RunProvenance for PipelineRun {
    type Error = crate::plan_fakes::FakeError;

    fn workflow_run(&self, _run_id: NonZeroU64) -> Result<Option<WorkflowRunFacts>, Self::Error> {
        Ok(Some(self.0.clone()))
    }
}

/// The default scenario's run over `world`'s snapshot, with `edit` applied before it is
/// published, then read back as decide-only reads it.
pub(crate) fn evidence(
    world: &World,
    edit: impl FnOnce(&mut Run) -> Result<(), Box<dyn Error>>,
) -> Result<EvidenceBundle, Box<dyn Error>> {
    evidence_of(&Scenario::new()?, world, edit)
}

/// Like [`evidence`], for `scenario`'s run.
pub(crate) fn evidence_of(
    scenario: &Scenario,
    world: &World,
    edit: impl FnOnce(&mut Run) -> Result<(), Box<dyn Error>>,
) -> Result<EvidenceBundle, Box<dyn Error>> {
    Ok(stored_evidence(scenario, world, edit)?.bundle)
}

/// An evidence bundle as the artifact store holds it, and as decide-only reads it.
#[derive(Debug)]
pub(crate) struct StoredEvidence {
    /// The bundle's bytes, as uploaded.
    pub(crate) bytes: Vec<u8>,
    /// The evidence pointer the pipeline recorded for them.
    pub(crate) pointer: EvidencePointer,
    /// The pipeline's `pull_request_target` run that uploaded them.
    pub(crate) run: WorkflowRunFacts,
    /// The bundle, read back by `read_evidence`.
    pub(crate) bundle: EvidenceBundle,
}

/// Like [`evidence_of`], keeping the bytes, the pointer and the run beside the bundle.
pub(crate) fn stored_evidence(
    scenario: &Scenario,
    world: &World,
    edit: impl FnOnce(&mut Run) -> Result<(), Box<dyn Error>>,
) -> Result<StoredEvidence, Box<dyn Error>> {
    let mut run = Run::of(scenario)?;
    run.github = world.state()?;
    edit(&mut run)?;
    let published = stamped()?;
    run.publish(&published)?;
    let bytes = {
        let uploads = published.uploads.borrow();
        let [(_, _, bytes)] = uploads.as_slice() else {
            return Err(format!("expected one upload, got {}", uploads.len()).into());
        };
        bytes.clone()
    };
    let run_uid = run.request.plan.run_uid;
    let pointer = EvidencePointer {
        run_id: run_uid.run_id,
        run_attempt: NonZeroU64::MIN,
        sha256: Sha256Digest::of(&bytes),
    };
    let facts = WorkflowRunFacts {
        run_id: run_uid.run_id,
        repo_id: run_uid.repo_id,
        path: ".github/workflows/assay.yml".to_owned(),
        event: RunEvent::PullRequestTarget,
        head_branch: "feature".to_owned(),
        head_repo: Some(run_uid.repo_id),
        run_attempt: NonZeroU64::MIN,
    };
    let rubric = rubric_v1()?;
    let request = EvidenceRequest {
        repo_id: run_uid.repo_id,
        pointer: &pointer,
        default_branch: "main",
        rubric: &rubric,
    };
    let read = read_evidence(
        &request,
        &PipelineRun(facts.clone()),
        &FakeBundles::new(Stored::Bytes(bytes.clone())),
    )?;
    match read {
        EvidenceRead::Authentic(bundle) => Ok(StoredEvidence {
            bytes,
            pointer,
            run: facts,
            bundle: *bundle,
        }),
        other => Err(format!("authentic evidence, got {other:?}").into()),
    }
}

/// What `redecided_state` returns over the fakes.
pub(crate) type Redecided =
    Result<GitHubState, RedecideStateError<FakeError, FakeError, FakeError>>;

/// The re-decision snapshot of `world`'s PR from `evidence`, with the base tip's lists `tip`.
pub(crate) fn redecided(
    world: &World,
    evidence: &EvidenceBundle,
    tip: &EligibilityLists,
) -> Redecided {
    redecided_state(
        &RedecidePorts {
            forge: world,
            ledger: world,
            comments: world,
        },
        &RedecideRequest {
            number: world.number,
            recorded: evidence.recorded(),
            tip,
        },
    )
}

/// The base tip's lists when `.assay/owners` reads `owners` and `.assay/agents` is unchanged.
pub(crate) fn tip_lists(owners: &str) -> Result<EligibilityLists, Box<dyn Error>> {
    Ok(EligibilityLists::parse(Some(owners), Some(LISTS_AGENTS))?)
}
