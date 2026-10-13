//! Every gate the recomputed plan schedules, as `decide` reads it (04 §5 rule 1): what its job
//! reported, its document authenticated and validated, and its findings fingerprinted by Z4. A
//! required gate that did not report is never a pass.

use std::collections::{BTreeMap, BTreeSet};

use assay_domain::{
    FindingsDocument, GateConclusion, GateFinding, GateOutcome, GateReport, SourceKind,
    StepAttribution,
};
use serde_json::Value;

use super::evidence::{Anchors, Cited, read_texts};
use super::fetch::{Artifact, Fetch};
use super::ports::RunArtifacts;
use super::recompute::AuthenticPlan;
use super::refusal::{OutputsProblem, PlanMismatch, Refusal};
use super::request::{DecideRequest, GateJobOutputs, JobResult, Reported, UpstreamJob};
use super::{Halt, Reader, RunDocument};
use crate::config::{Plugin, Sha256Digest};
use crate::contract::{Contract, ContractValidator};
use crate::gate::{BuildJob, Conclusion, GateError, GateJob, scheduled};
use crate::plan::{ArtifactJob, PullRequestObjects};

/// The gates of a run, with the gate findings as Z4 anchored them.
#[derive(Debug, Default)]
pub(super) struct Gates {
    pub(super) reports: Vec<GateReport>,
    pub(super) findings: Vec<assay_domain::Finding>,
}

/// One scheduled gate before its findings are anchored.
struct Pending<'p> {
    plugin: &'p Plugin,
    state: State,
}

/// What a gate's job and document say.
enum State {
    /// Settled without a document.
    Settled(GateOutcome),
    /// Ran, with an authenticated, schema-valid document.
    Ran {
        conclusion: GateConclusion,
        findings: Vec<assay_domain::Finding>,
    },
}

/// Reads every scheduled gate of every gates job.
pub(super) fn gates<O: PullRequestObjects, S, A: RunArtifacts>(
    request: &DecideRequest,
    plan: &AuthenticPlan,
    reader: &mut Reader<'_, O, A>,
) -> Result<Gates, Halt<O::Error, S, A::Error>> {
    let jobs = [
        (GateJob::Static, &request.gates_static),
        (GateJob::Build(BuildJob::Lint), &request.gates_lint),
        (GateJob::Build(BuildJob::Test), &request.gates_test),
    ];
    let mut pending = Vec::new();
    for (job, outputs) in jobs {
        let plugins: Vec<&Plugin> = plan
            .recomputed
            .config
            .plugins
            .plugins()
            .iter()
            .filter(|plugin| scheduled(plugin, job))
            .collect();
        let reports = reports_by_plugin(job, outputs, &plugins)?;
        let artifact = plan
            .artifact_names
            .get(&artifact_job(job))
            .map(|name| Artifact {
                job: artifact_job(job),
                name,
            });
        for plugin in plugins {
            let state = match (reports.get(plugin.name().as_str()), artifact) {
                (
                    Some(Reported::Concluded {
                        conclusion,
                        document,
                    }),
                    Some(artifact),
                ) => {
                    let reported = Report {
                        conclusion: *conclusion,
                        artifact,
                        digest: document,
                    };
                    documented(&reported, plugin, reader)?
                }
                (Some(Reported::Error(error)), _) => State::Settled(errored(&error.detail())),
                (Some(Reported::Concluded { .. }), None) => {
                    return Err(PlanMismatch::ArtifactNames.into());
                }
                (None, _) => {
                    let Some(outcome) = unreported(outputs.result) else {
                        let problem = OutputsProblem::MissingPlugin;
                        let job = upstream_job(job);
                        return Err(Refusal::Outputs { job, problem }.into());
                    };
                    State::Settled(outcome)
                }
            };
            pending.push(Pending { plugin, state });
        }
    }
    anchored(request, plan, reader.objects, pending)
}

/// What a gate's job reported with a document: its conclusion, the job's artifact and the
/// digest the job published for the document.
struct Report<'r> {
    conclusion: Conclusion,
    artifact: Artifact<'r>,
    digest: &'r Sha256Digest,
}

/// The state of a gate whose job `reported` a document. A document whose bytes are not the
/// published ones refuses the run (F18); authentic ones, valid or not, are kept for the bundle.
fn documented<O: PullRequestObjects, S, A: RunArtifacts>(
    reported: &Report<'_>,
    plugin: &Plugin,
    reader: &mut Reader<'_, O, A>,
) -> Result<State, Halt<O::Error, S, A::Error>> {
    let file = format!("{}.json", plugin.name());
    let fetched = reader
        .fetcher
        .fetch(reported.artifact, &file, reported.digest)?;
    let bytes = match fetched {
        Fetch::Authentic(bytes) => bytes,
        Fetch::Tampered => {
            let job = reported.artifact.job;
            return Err(Refusal::ArtifactDigest { job, file }.into());
        }
        Fetch::Missing => return Ok(State::Settled(errored("artifact-missing"))),
        Fetch::TooLarge => {
            let detail = GateError::DocumentTooLarge.detail();
            return Ok(State::Settled(errored(&detail)));
        }
    };
    let state = read_document(&bytes, plugin, reported.conclusion, reader.validator);
    reader.kept.push(RunDocument {
        job: reported.artifact.job,
        file,
        // The bundle records the digest beside the bytes; the request keeps its own.
        digest: reported.digest.clone(),
        bytes,
    });
    Ok(state)
}

/// The job's reports by plugin, refusing outputs that name a plugin base config does not
/// schedule there, name one twice, or come from a skipped job.
fn reports_by_plugin<'o>(
    job: GateJob,
    outputs: &'o GateJobOutputs,
    plugins: &[&Plugin],
) -> Result<BTreeMap<&'o str, &'o Reported>, Refusal> {
    let refuse = |problem| Refusal::Outputs {
        job: upstream_job(job),
        problem,
    };
    if outputs.result == JobResult::Skipped && !outputs.gates.is_empty() {
        return Err(refuse(OutputsProblem::SkippedJobReported));
    }
    let known: BTreeSet<&str> = plugins
        .iter()
        .map(|plugin| plugin.name().as_str())
        .collect();
    let mut reports = BTreeMap::new();
    for report in &outputs.gates {
        let plugin = report.plugin.as_str();
        if !known.contains(plugin) {
            return Err(refuse(OutputsProblem::UnknownPlugin));
        }
        if reports.insert(plugin, &report.reported).is_some() {
            return Err(refuse(OutputsProblem::DuplicatePlugin));
        }
    }
    Ok(reports)
}

/// A gate its job did not report: skipped when the job was, an error when the job failed
/// before reporting it, and `None` when the job succeeded without it, whose outputs are then
/// refused (a cancelled job never gets here: it stops `authenticate` first).
fn unreported(result: JobResult) -> Option<GateOutcome> {
    match result {
        JobResult::Failure => Some(errored("job-failed")),
        JobResult::Skipped | JobResult::Cancelled => Some(GateOutcome::Skipped),
        JobResult::Success => None,
    }
}

fn errored(detail: &str) -> GateOutcome {
    GateOutcome::Error {
        detail: detail.to_owned(),
    }
}

/// The state of a gate from its authenticated document: `invalid-document` unless it is a
/// schema-valid gate document from `plugin` whose conclusion agrees with the job's, where a
/// failure on either side is a failure.
fn read_document(
    bytes: &[u8],
    plugin: &Plugin,
    reported: Conclusion,
    validator: &ContractValidator,
) -> State {
    let invalid = || State::Settled(errored(&GateError::InvalidDocument.detail()));
    let Some(document) = validator
        .read(Contract::Findings, bytes)
        .ok()
        .and_then(|value| gate_document(value, plugin))
    else {
        return invalid();
    };
    let Some(written) = document.conclusion else {
        return invalid();
    };
    let conclusion = match (conclusion(reported), written) {
        (GateConclusion::Fail, _) | (_, GateConclusion::Fail) => GateConclusion::Fail,
        (from_job, written) if from_job == written => written,
        // The job and the document disagree on a conclusion that is not a failure.
        _ => return invalid(),
    };
    State::Ran {
        conclusion,
        findings: document.findings,
    }
}

/// A schema-valid document as `plugin`'s gate document, if it is one.
fn gate_document(value: Value, plugin: &Plugin) -> Option<FindingsDocument> {
    let document: FindingsDocument = serde_json::from_value(value).ok()?;
    (document.source.kind == SourceKind::Gate && document.source.name == plugin.name().as_str())
        .then_some(document)
}

const fn conclusion(conclusion: Conclusion) -> GateConclusion {
    match conclusion {
        Conclusion::Pass => GateConclusion::Pass,
        Conclusion::Fail => GateConclusion::Fail,
        Conclusion::Neutral => GateConclusion::Neutral,
    }
}

/// The reports, with every finding of a gate that ran anchored on the trees.
fn anchored<O: PullRequestObjects, S, A>(
    request: &DecideRequest,
    plan: &AuthenticPlan,
    objects: &O,
    pending: Vec<Pending<'_>>,
) -> Result<Gates, Halt<O::Error, S, A>> {
    let mut cited = Cited::default();
    for gate in &pending {
        if let State::Ran { findings, .. } = &gate.state {
            for finding in findings {
                cited.add(finding);
            }
        }
    }
    let pull_request = &request.plan.pull_request;
    let texts = read_texts(
        objects,
        (&pull_request.base_sha, &pull_request.head_sha),
        &plan.recomputed.files,
        &cited,
    )?;
    let mut anchors = Anchors::new(&texts);
    let mut gates = Gates::default();
    for gate in pending {
        let (outcome, findings) = match gate.state {
            State::Settled(outcome) => (outcome, Vec::new()),
            State::Ran {
                conclusion,
                mut findings,
            } => {
                let outcome = GateOutcome::Ran {
                    conclusion,
                    steps: StepAttribution::Unknown,
                };
                let fingerprints: Option<Vec<GateFinding>> = findings
                    .iter_mut()
                    .map(|finding| {
                        let fp = anchors.anchor(finding)?;
                        Some(GateFinding {
                            fp,
                            // The report and the anchored finding each own the rule.
                            rule: finding.rule.clone(),
                            severity: finding.severity,
                        })
                    })
                    .collect();
                match fingerprints {
                    Some(reported) => {
                        gates.findings.append(&mut findings);
                        (outcome, reported)
                    }
                    None => (errored("evidence-missing"), Vec::new()),
                }
            }
        };
        gates.reports.push(GateReport {
            name: gate.plugin.name().to_string(),
            required: gate.plugin.required(),
            outcome,
            findings,
        });
    }
    Ok(gates)
}

const fn artifact_job(job: GateJob) -> ArtifactJob {
    match job {
        GateJob::Static => ArtifactJob::GatesStatic,
        GateJob::Build(BuildJob::Lint) => ArtifactJob::GatesLint,
        GateJob::Build(BuildJob::Test) => ArtifactJob::GatesTest,
    }
}

const fn upstream_job(job: GateJob) -> UpstreamJob {
    match job {
        GateJob::Static => UpstreamJob::GatesStatic,
        GateJob::Build(BuildJob::Lint) => UpstreamJob::GatesLint,
        GateJob::Build(BuildJob::Test) => UpstreamJob::GatesTest,
    }
}
