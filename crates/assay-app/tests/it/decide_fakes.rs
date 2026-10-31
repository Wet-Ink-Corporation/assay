//! In-memory fakes of the `decide` ports (TST-09) and a scenario builder: a PR planned by the
//! real `plan`, its artifacts uploaded with the digests their jobs would publish.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    AdmitJob, AdmitOutcome, ArtifactDocument, ArtifactJob, AuthenticatedRun, Authentication,
    CommitId, ContractValidator, CurrentPullRequest, DecideError, DecidePorts, DecideRequest,
    Fetched, GateJobOutputs, JobResult, PlanJob, PlanRequest, PullOnHead, PullRequestState,
    Refusal, Reported, ReportedGate, ReviewJob, ReviewStage, RunArtifacts, Sha256Digest,
    TierDegrade, authenticate, plan,
};
use assay_domain::{Aeu, GateConclusion, RunUid, Tier};
use serde_json::{Value, json};

use crate::plan_fakes::{BASE, CountingNonces, FakeError, FakePr, HEAD, plugins_toml, request};

/// The run's artifacts, by artifact name and file.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeArtifacts {
    files: BTreeMap<(String, String), Vec<u8>>,
    /// Every fetch fails.
    pub(crate) failing: bool,
    /// Fetches return the whole file whatever the limit, as a careless adapter would.
    pub(crate) ignore_limit: bool,
    /// Every fetch, in order: the limit asked for and the bytes returned, if any.
    pub(crate) fetches: RefCell<Vec<(usize, Option<usize>)>>,
}

impl FakeArtifacts {
    pub(crate) fn put(&mut self, artifact: &str, file: &str, bytes: Vec<u8>) {
        self.files
            .insert((artifact.to_owned(), file.to_owned()), bytes);
    }

    pub(crate) fn remove(&mut self, artifact: &str, file: &str) {
        self.files.remove(&(artifact.to_owned(), file.to_owned()));
    }
}

impl RunArtifacts for FakeArtifacts {
    type Error = FakeError;

    fn fetch(&self, artifact: &str, file: &str, limit: usize) -> Result<Fetched, FakeError> {
        if self.failing {
            return Err(FakeError);
        }
        let fetched = match self.files.get(&(artifact.to_owned(), file.to_owned())) {
            None => Fetched::Missing,
            Some(bytes) if bytes.len() > limit && !self.ignore_limit => Fetched::TooLarge,
            Some(bytes) => Fetched::Found(bytes.clone()),
        };
        let returned = match &fetched {
            Fetched::Found(bytes) => Some(bytes.len()),
            Fetched::Missing | Fetched::TooLarge => None,
        };
        self.fetches.borrow_mut().push((limit, returned));
        Ok(fetched)
    }
}

/// GitHub's view of the PR at decide time.
#[derive(Debug, Clone)]
pub(crate) struct FakeState {
    pub(crate) current: CurrentPullRequest,
    pub(crate) pulls: Vec<PullOnHead>,
    pub(crate) newer_run: bool,
    /// Every call fails.
    pub(crate) failing: bool,
}

impl PullRequestState for FakeState {
    type Error = FakeError;

    fn current(&self, _number: NonZeroU64) -> Result<CurrentPullRequest, FakeError> {
        if self.failing {
            return Err(FakeError);
        }
        Ok(self.current.clone())
    }

    fn pulls_on_head(&self, _head: &CommitId) -> Result<Vec<PullOnHead>, FakeError> {
        if self.failing {
            return Err(FakeError);
        }
        Ok(self.pulls.clone())
    }

    fn newer_run(&self, _run: RunUid, _head: &CommitId) -> Result<bool, FakeError> {
        if self.failing {
            return Err(FakeError);
        }
        Ok(self.newer_run)
    }
}

/// A gate document from `gate`, concluding `conclusion` with `findings`.
pub(crate) fn gate_document(gate: &str, conclusion: &str, findings: &Value) -> Vec<u8> {
    let document = json!({
        "schema": "assay.findings/1",
        "source": {"kind": "gate", "name": gate, "version": "git:x", "run_key": "run-42-1"},
        "conclusion": conclusion,
        "findings": findings,
    });
    document.to_string().into_bytes()
}

/// One gate finding of rule `rule` citing `file:line` with `evidence`.
pub(crate) fn gate_finding(rule: &str, file: &str, line: u64, evidence: &str) -> Value {
    json!({
        "class": "error-handling", "rule": rule, "kind": "defect", "severity": "blocker",
        "file": file, "line": line, "evidence": evidence,
        "title": "used `unwrap()`", "body": "H-02: no unwrap in production code.",
    })
}

/// The outputs of a run whose jobs all succeeded, each scheduled gate passing.
fn passing_request(plan: PlanRequest, plan_job: PlanJob) -> DecideRequest {
    let passing = |plugin: &str| GateJobOutputs {
        result: JobResult::Success,
        gates: vec![ReportedGate {
            plugin: plugin.to_owned(),
            reported: Reported::Concluded {
                conclusion: GateConclusion::Pass,
                document: Sha256Digest::of(&gate_document(plugin, "pass", &json!([]))),
            },
        }],
    };
    DecideRequest {
        plan,
        plan_job,
        gates_static: passing("temper"),
        gates_lint: passing("temper"),
        gates_test: passing("custom-lint"),
        admit: AdmitJob {
            result: JobResult::Success,
            degrade: TierDegrade::None,
            // The default PR's tier; `authenticate` does not read the admission.
            admission: AdmitOutcome::Spend { tier: Tier::Micro },
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

/// GitHub showing PR 42 at the planned head and base, the only open PR on its head.
fn unmoved() -> Result<FakeState, Box<dyn Error>> {
    Ok(FakeState {
        current: CurrentPullRequest {
            head_sha: HEAD.parse()?,
            base_ref: "main".to_owned(),
        },
        pulls: vec![PullOnHead {
            number: NonZeroU64::new(42).ok_or("42 is not zero")?,
            open: true,
            base_ref: "main".to_owned(),
        }],
        newer_run: false,
        failing: false,
    })
}

/// What `authenticate` returns over the fakes.
pub(crate) type Outcome = Result<Authentication, DecideError<FakeError, FakeError, FakeError>>;

/// The authenticated run of `outcome`, or why there is none.
pub(crate) fn authenticated(outcome: Outcome) -> Result<AuthenticatedRun, Box<dyn Error>> {
    match outcome? {
        Authentication::Authenticated(run) => Ok(*run),
        other => Err(format!("expected an authenticated run, got {other:?}").into()),
    }
}

/// The refusal of `outcome`, or what it was instead.
pub(crate) fn refusal(outcome: Outcome) -> Result<Refusal, Box<dyn Error>> {
    match outcome? {
        Authentication::Refused(refusal) => Ok(refusal),
        other => Err(format!("expected a refusal, got {other:?}").into()),
    }
}

/// PR 42 planned by `plan`, every artifact uploaded with its digest in its job's outputs: each
/// scheduled gate passes, and GitHub still shows the PR at the planned head and base.
#[derive(Debug, Clone)]
pub(crate) struct Scenario {
    pub(crate) pr: FakePr,
    pub(crate) request: DecideRequest,
    pub(crate) state: FakeState,
    pub(crate) artifacts: FakeArtifacts,
    pub(crate) names: BTreeMap<ArtifactJob, String>,
    pub(crate) plan_json: Vec<u8>,
}

impl Scenario {
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        Self::of(FakePr::new())
    }

    /// The scenario for `pr`, planned with a base config that protects `main`.
    pub(crate) fn of(pr: FakePr) -> Result<Self, Box<dyn Error>> {
        Self::requested(pr, |_| {})
    }

    /// The scenario for `pr`, its plan request changed by `adjust` before planning.
    pub(crate) fn requested(
        mut pr: FakePr,
        adjust: impl FnOnce(&mut PlanRequest),
    ) -> Result<Self, Box<dyn Error>> {
        let protected = plugins_toml().replacen(
            "claude_cli = \"2.1.296\"",
            "claude_cli = \"2.1.296\"\nprotected_bases = [\"main\"]",
            1,
        );
        pr.write(BASE, ".assay/plugins.toml", protected.as_bytes());
        let mut plan_request = request(&pr)?;
        adjust(&mut plan_request);
        let planned = plan(
            &plan_request,
            &pr,
            &mut CountingNonces::default(),
            &ContractValidator::new()?,
        )?;
        let names = planned.artifact_names().clone();
        let plan_artifact = names
            .get(&ArtifactJob::Plan)
            .ok_or("a plan artifact")?
            .clone();
        let plan_json = planned.json().to_vec();
        let plan_job = PlanJob {
            result: JobResult::Success,
            artifact: plan_artifact.clone(),
            document: Sha256Digest::of(&plan_json),
        };
        let request = passing_request(plan_request, plan_job);
        let mut artifacts = FakeArtifacts::default();
        artifacts.put(&plan_artifact, "run.json", plan_json.clone());
        for (job, plugin) in [
            (ArtifactJob::GatesStatic, "temper"),
            (ArtifactJob::GatesLint, "temper"),
            (ArtifactJob::GatesTest, "custom-lint"),
        ] {
            let artifact = names.get(&job).ok_or("a gate artifact")?;
            let file = format!("{plugin}.json");
            artifacts.put(artifact, &file, gate_document(plugin, "pass", &json!([])));
        }
        Ok(Self {
            pr,
            request,
            state: unmoved()?,
            artifacts,
            names,
            plan_json,
        })
    }

    /// The name of `job`'s artifact.
    pub(crate) fn artifact(&self, job: ArtifactJob) -> Result<String, Box<dyn Error>> {
        Ok(self
            .names
            .get(&job)
            .ok_or("every job has an artifact")?
            .clone())
    }

    /// Replaces `job`'s document for `plugin` with `bytes` and publishes their digest with
    /// `conclusion`, as an honest job would.
    pub(crate) fn publish_gate(
        &mut self,
        (job, plugin): (ArtifactJob, &str),
        conclusion: GateConclusion,
        bytes: Vec<u8>,
    ) -> Result<(), Box<dyn Error>> {
        let digest = Sha256Digest::of(&bytes);
        let artifact = self.artifact(job)?;
        self.artifacts
            .put(&artifact, &format!("{plugin}.json"), bytes);
        let outputs = match job {
            ArtifactJob::GatesStatic => &mut self.request.gates_static,
            ArtifactJob::GatesLint => &mut self.request.gates_lint,
            _ => &mut self.request.gates_test,
        };
        outputs.gates.retain(|gate| gate.plugin != plugin);
        outputs.gates.push(ReportedGate {
            plugin: plugin.to_owned(),
            reported: Reported::Concluded {
                conclusion,
                document: digest,
            },
        });
        Ok(())
    }

    /// Uploads `bytes` as `file` of the review artifact and publishes their digest, as an
    /// honest `review` job would.
    pub(crate) fn publish_review(
        &mut self,
        file: &str,
        bytes: Vec<u8>,
    ) -> Result<(), Box<dyn Error>> {
        self.request.review.documents.push(ArtifactDocument {
            file: file.to_owned(),
            digest: Sha256Digest::of(&bytes),
        });
        let artifact = self.artifact(ArtifactJob::Review)?;
        self.artifacts.put(&artifact, file, bytes);
        Ok(())
    }

    /// Replaces the plan document with `bytes` and publishes their digest, as a plan job that
    /// wrote them would.
    pub(crate) fn publish_plan(&mut self, bytes: Vec<u8>) {
        self.request.plan_job.document = Sha256Digest::of(&bytes);
        let artifact = self.request.plan_job.artifact.clone();
        self.artifacts.put(&artifact, "run.json", bytes);
    }

    /// The plan document, parsed.
    pub(crate) fn plan_value(&self) -> Result<Value, Box<dyn Error>> {
        Ok(serde_json::from_slice(&self.plan_json)?)
    }

    pub(crate) fn authenticate(&self) -> Result<Outcome, Box<dyn Error>> {
        let ports = DecidePorts {
            objects: &self.pr,
            state: &self.state,
            artifacts: &self.artifacts,
        };
        Ok(authenticate(
            &self.request,
            &ports,
            &ContractValidator::new()?,
        ))
    }
}
