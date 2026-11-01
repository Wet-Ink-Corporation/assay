//! `run_decide` (#60 ST-14, Z4, D5 A) over fakes and a fixture PR planned by `run_plan`, with
//! the artifacts laid out as `download-artifact` lays them out: authentic artifacts give a
//! bundle and its digest, a tampered gate document a refusal and no bundle, and a superseded
//! head nothing to publish.

use std::cell::RefCell;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{
    AdmitJob, AdmitOutcome, ArtifactJob, BUNDLE_FILE, ContractValidator, CurrentPullRequest,
    GateJobOutputs, JobResult, Reported, ReportedGate, RestFile, RestFiles, ReviewJob, ReviewStage,
    Sha256Digest, TierDegrade, UserId, read_rubric,
};
use assay_cli::outputs::{DecideNeeds, DecideOutputs, Identity, PlanEnd, PlanOutputs, Trigger};
use assay_cli::{
    DecideJob, DecideJobPorts, DecideRunError, DecideState, DirArtifacts, FirstAttempt,
    ForgePullRequest, OutDir, PlanJob, PlanPorts, RUN_FILE, RunEnv, RunVariables, STATE_FILE,
    run_decide, run_plan,
};
use assay_domain::{Aeu, ContentHash, GateConclusion, ReasonCode, Rubric};
use assay_forge::git::TreeLimits;
use serde_json::json;
use tempfile::TempDir;

use crate::admit_fakes::{Fetches, NoBundles, NoComments};
use crate::decide_fakes::{LiveState, Reviewless};
use crate::open_fakes::{MemoryLedger, Refused};
use crate::plan::{Counting, Pr, pr};

/// The PR's number and its author.
const PR: u64 = 55;
const AUTHOR: u64 = 7;

fn id(n: u64) -> Result<NonZeroU64, Box<dyn Error>> {
    Ok(NonZeroU64::new(n).ok_or("a positive id")?)
}

/// rubric v1, as the binary embeds it.
pub(crate) fn rubric() -> Result<Rubric, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rubric/v1.toml");
    Ok(read_rubric(&fs::read(path)?)?)
}

/// A temper gate document concluding `pass` with no finding.
fn temper_document() -> Vec<u8> {
    json!({
        "schema": "assay.findings/1",
        "source": {"kind": "gate", "name": "temper", "version": "git:x", "run_key": "run-42-1"},
        "conclusion": "pass",
        "findings": [],
    })
    .to_string()
    .into_bytes()
}

/// A planned PR changing the library, with every upstream job's outputs and the artifacts
/// downloaded under `artifacts`.
pub(crate) struct Planned {
    pub(crate) pr: Pr,
    _scratch: TempDir,
    pub(crate) root: PathBuf,
    pub(crate) run: FirstAttempt,
    pub(crate) needs: DecideNeeds,
    pub(crate) validator: ContractValidator,
    pub(crate) rubric: Rubric,
    pub(crate) plan: PlanOutputs,
}

impl Planned {
    /// A ready PR planned as `plan` would, its gates passing and admitted to spend, with the
    /// placeholder review.
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        Self::with_base(&[])
    }

    /// Like [`Self::new`], with `base_files` also in the base commit.
    pub(crate) fn with_base(base_files: &[(&str, &str)]) -> Result<Self, Box<dyn Error>> {
        let pr = pr(base_files, &[("src/lib.rs", "pub fn two() {}\n")])?;
        let run = RunEnv::from_vars(&RunVariables {
            run_id: Some(OsString::from("4242")),
            run_attempt: Some(OsString::from("1")),
            repository_id: Some(OsString::from("42")),
            repository: Some(OsString::from("Wet-Ink-Corporation/assay")),
        })?;
        let identity = identity(&pr)?;
        let scratch = tempfile::tempdir()?;
        let root = fs::canonicalize(scratch.path())?;
        let plan_out = OutDir::new(&root.join("plan-out"))?;
        let validator = ContractValidator::new()?;
        let objects =
            ForgePullRequest::new(&pr.repo, TreeLimits::default(), Duration::from_secs(60));
        let plan = run_plan(
            PlanJob {
                run: &run,
                // plan's job owns its identity; decide's needs take the original.
                identity: identity.clone(),
                rest_files: rest_files(),
                prompt_bundle: ContentHash::of_files([]),
            },
            PlanPorts {
                objects: &objects,
                nonces: &mut Counting::default(),
                validator: &validator,
            },
            &plan_out,
        )?;
        download(&root.join("artifacts"), &plan_out, &plan)?;
        Ok(Self {
            pr,
            _scratch: scratch,
            root,
            run: run.first_attempt()?,
            needs: needs(identity, &plan),
            validator,
            rubric: rubric()?,
            plan,
        })
    }

    /// Where the artifacts were downloaded.
    pub(crate) fn artifacts(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    /// decide's `--out`.
    pub(crate) fn out(&self) -> PathBuf {
        self.root.join("decide-out")
    }

    /// GitHub showing the PR at the planned head and base.
    pub(crate) fn unmoved(&self) -> LiveState {
        LiveState {
            current: CurrentPullRequest {
                head_sha: self.needs.identity.head_sha.clone(),
                base_ref: self.needs.identity.base_ref.clone(),
            },
            calls: RefCell::default(),
        }
    }

    /// Replaces gates-static's downloaded `temper.json` with a document it did not publish.
    pub(crate) fn tamper_gates_static(&self) -> Result<(), Box<dyn Error>> {
        let artifact = self
            .plan
            .artifacts()
            .get(&ArtifactJob::GatesStatic)
            .ok_or("a gates-static artifact")?;
        fs::write(
            self.artifacts().join(artifact).join("temper.json"),
            b"{\"forged\":true}",
        )?;
        Ok(())
    }

    /// Decides over `state` and the downloaded artifacts, with an empty ledger and no
    /// previous run.
    pub(crate) fn decide(&self, state: &LiveState) -> Result<Ran, Box<dyn Error>> {
        self.decide_over(state, &MemoryLedger::default())
    }

    /// Like [`Self::decide`], over the ledger files `ledger` holds.
    pub(crate) fn decide_over(
        &self,
        state: &LiveState,
        ledger: &MemoryLedger,
    ) -> Result<Ran, Box<dyn Error>> {
        let objects = ForgePullRequest::new(
            &self.pr.repo,
            TreeLimits::default(),
            Duration::from_secs(60),
        );
        let forge = Reviewless::by(UserId(id(AUTHOR)?));
        let artifacts = DirArtifacts::new(self.artifacts());
        let ports = DecideJobPorts {
            fetch: &Fetches::default(),
            objects: &objects,
            state,
            artifacts: &artifacts,
            forge: &forge,
            ledger,
            bundles: &NoBundles::default(),
            comments: &NoComments,
        };
        let job = DecideJob {
            run: &self.run,
            // The job owns its needs; each test decides once, `self` keeps the original.
            needs: self.needs.clone(),
            rest_files: rest_files(),
            prompt_bundle: ContentHash::of_files([]),
            rubric: &self.rubric,
            validator: &self.validator,
        };
        Ok(run_decide(job, &ports, &OutDir::new(&self.out())?))
    }
}

/// What `run_decide` gives over the fakes.
pub(crate) type Ran = Result<
    DecideOutputs,
    DecideRunError<
        Refused,
        Refused,
        assay_cli::DirArtifactsError,
        Refused,
        Refused,
        Refused,
        Refused,
    >,
>;

/// GitHub showing the PR moved to another head.
pub(crate) fn moved() -> Result<LiveState, Box<dyn Error>> {
    Ok(LiveState {
        current: CurrentPullRequest {
            head_sha: "9999999999999999999999999999999999999999".parse()?,
            base_ref: "main".to_owned(),
        },
        calls: RefCell::default(),
    })
}

/// guard's identity of the fixture PR, ready.
fn identity(pr: &Pr) -> Result<Identity, Box<dyn Error>> {
    Ok(Identity {
        pr: id(PR)?,
        head_sha: pr.head.as_str().parse()?,
        base_sha: pr.base.as_str().parse()?,
        base_ref: "main".to_owned(),
        draft: false,
        author: UserId(id(AUTHOR)?),
        sender: UserId(id(AUTHOR)?),
        trigger: Trigger::Opened,
        default_branch: "main".to_owned(),
    })
}

/// Lays the artifacts out under `artifacts` as `download-artifact` does: plan's `run.json`
/// from `plan_out`, and a passing temper document for gates-static and gates-lint.
fn download(artifacts: &Path, plan_out: &OutDir, plan: &PlanOutputs) -> Result<(), Box<dyn Error>> {
    let artifact = |job| plan.artifacts().get(&job).ok_or("an artifact name");
    let plan_dir = artifacts.join(artifact(ArtifactJob::Plan)?);
    fs::create_dir_all(&plan_dir)?;
    fs::copy(plan_out.path().join(RUN_FILE), plan_dir.join(RUN_FILE))?;
    for job in [ArtifactJob::GatesStatic, ArtifactJob::GatesLint] {
        let dir = artifacts.join(artifact(job)?);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("temper.json"), temper_document())?;
    }
    Ok(())
}

/// Every upstream job's outputs as decide reads them: `plan`'s, the static and lint gates
/// passing, admit spending at plan's tier, and the placeholder review.
fn needs(identity: Identity, plan: &PlanOutputs) -> DecideNeeds {
    let passing = GateJobOutputs {
        result: JobResult::Success,
        gates: vec![ReportedGate {
            plugin: "temper".to_owned(),
            reported: Reported::Concluded {
                conclusion: GateConclusion::Pass,
                document: Sha256Digest::of(&temper_document()),
            },
        }],
    };
    DecideNeeds {
        identity,
        // The needs own a copy of plan's outputs; the fixture keeps them for artifact names.
        plan: PlanEnd::Succeeded(plan.clone()),
        // Both gate jobs report the same passing gate.
        gates_static: passing.clone(),
        gates_lint: passing,
        gates_test: GateJobOutputs {
            result: JobResult::Success,
            gates: Vec::new(),
        },
        admit: AdmitJob {
            result: JobResult::Success,
            degrade: TierDegrade::None,
            admission: AdmitOutcome::Spend { tier: plan.tier() },
        },
        review: ReviewJob {
            result: JobResult::Success,
            stage: ReviewStage::NotRun,
            cost: Aeu::from_micros(0),
            documents: Vec::new(),
        },
        release: JobResult::Success,
    }
}

/// GitHub's file list of the fixture PR.
fn rest_files() -> RestFiles {
    RestFiles {
        changed_files: 1,
        files: vec![RestFile {
            path: "src/lib.rs".to_owned(),
            previous_path: None,
        }],
    }
}

/// The state decide wrote.
fn state(out: &Path) -> Result<DecideState, Box<dyn Error>> {
    Ok(DecideState::read(&fs::read(out.join(STATE_FILE))?)?)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn authentic_artifacts_give_a_bundle_and_its_digest() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new()?;

    let outputs = planned.decide(&planned.unmoved())??;

    let bundle = fs::read(planned.out().join(BUNDLE_FILE))?;
    let digest = Sha256Digest::of(&bundle);
    assert_eq!(
        outputs,
        DecideOutputs::Bundle {
            digest: digest.clone()
        }
    );
    assert_eq!(state(&planned.out())?, DecideState::Decided { digest });
    let value: serde_json::Value = serde_json::from_slice(&bundle)?;
    assert_eq!(value["schema"], "assay.run-inputs/1");
    assert_eq!(value["run_uid"]["run_id"], 4242);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_tampered_gate_document_is_refused_and_writes_no_bundle() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new()?;
    planned.tamper_gates_static()?;

    let outputs = planned.decide(&planned.unmoved())??;

    assert_eq!(outputs, DecideOutputs::NoBundle);
    let DecideState::Refused(refusal) = state(&planned.out())? else {
        return Err("not a refusal".into());
    };
    assert_eq!(refusal.reason().code, ReasonCode::GateError);
    assert_eq!(
        refusal.reason().detail,
        "artifact-digest: gates-static/temper.json"
    );
    assert!(!planned.out().join(BUNDLE_FILE).exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_superseded_head_publishes_nothing_and_reads_no_further() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new()?;
    let moved = moved()?;

    let outputs = planned.decide(&moved)??;

    assert_eq!(outputs, DecideOutputs::NoBundle);
    assert_eq!(state(&planned.out())?, DecideState::Silent);
    assert!(!planned.out().join(BUNDLE_FILE).exists());
    assert_eq!(
        *moved.calls.borrow(),
        ["current"],
        "nothing read after the move"
    );
    Ok(())
}
