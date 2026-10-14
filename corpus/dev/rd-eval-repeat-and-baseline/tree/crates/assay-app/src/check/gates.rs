//! Local mode's gates: every plugin the base schedules, run job by job through the local
//! `GateRunner`, with their findings anchored on the snapshot as `decide` anchors them.

use assay_domain::{
    Finding, FindingsDocument, GateConclusion, GateFinding, GateOutcome, GateReport, SourceKind,
    StepAttribution,
};
use serde::Serialize;

use crate::config::PluginsConfig;
use crate::contract::ContractValidator;
use crate::decide::{Anchors, Cited, Texts};
use crate::gate::{
    self, BuildJob, Conclusion, GateError, GateJob, GateRunner, account_for_required, run_gates,
};

/// Every gate job, in pipeline order.
const JOBS: [GateJob; 3] = [
    GateJob::Static,
    GateJob::Build(BuildJob::Lint),
    GateJob::Build(BuildJob::Test),
];

/// One gate of the local run as the report shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalGate {
    /// The plugin.
    pub name: String,
    /// The job it ran in: `gates-static`, `gates-lint` or `gates-test`.
    pub job: &'static str,
    /// Whether base config requires it.
    pub required: bool,
    /// `pass`, `fail`, `neutral` or `gate-error:<detail>`.
    pub outcome: String,
    /// Why it is a gate error, in assay's words (`not run in local mode`); never gate output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// Its findings, anchored on the snapshot: a verified one quotes the tree's line.
    pub findings: Vec<Finding>,
}

/// One gate run before its findings are anchored.
pub(super) struct Ran {
    report: gate::GateReport,
    /// The document's findings; `None` when a concluded document did not read as this gate's.
    findings: Option<Vec<Finding>>,
}

impl Ran {
    /// The findings this gate cites, for the texts to read.
    pub(super) fn cite<'f>(&'f self, cited: &mut Cited<'f>) {
        for finding in self.findings.iter().flatten() {
            cited.add(finding);
        }
    }
}

/// The gates of the run, for the report and for `decide`.
#[derive(Debug, Default)]
pub(super) struct Gates {
    pub(super) local: Vec<LocalGate>,
    pub(super) reports: Vec<GateReport>,
    /// Every finding of a gate that ran, anchored: validation's `gate_findings`.
    pub(super) findings: Vec<Finding>,
}

/// Runs every plugin `config` schedules in each job, in job then file order. A required gate
/// the runner does not run is a gate error, never a pass.
pub(super) fn run<G: GateRunner>(
    config: &PluginsConfig,
    runner: &mut G,
    validator: &ContractValidator,
) -> Vec<Ran> {
    let mut ran = Vec::new();
    for job in JOBS {
        let mut reports = run_gates(config, job, runner, validator);
        account_for_required(config, job, &mut reports);
        ran.extend(reports.into_iter().map(read));
    }
    ran
}

/// A report with its validated document read as findings.
fn read(mut report: gate::GateReport) -> Ran {
    // `run_gates` validated the document; one that still does not read as this gate's is a
    // drift between schema and type, and the gate is an invalid-document error.
    let findings = report.document.take().map(|value| {
        match serde_json::from_value::<FindingsDocument>(value) {
            Ok(document)
                if document.source.kind == SourceKind::Gate
                    && document.source.name == report.plugin.as_str() =>
            {
                Some(document.findings)
            }
            Ok(_) | Err(_) => None,
        }
    });
    let findings = match (&report.outcome, findings) {
        (gate::GateOutcome::Concluded(_), Some(read)) => read,
        _ => Some(Vec::new()),
    };
    Ran { report, findings }
}

/// The reports with every finding anchored on `texts`; a gate with a finding that has no
/// content to anchor is `gate-error:evidence-missing`, as `decide` has it.
pub(super) fn anchor(ran: Vec<Ran>, texts: &Texts) -> Gates {
    let mut anchors = Anchors::new(texts);
    let mut gates = Gates::default();
    for Ran { report, findings } in ran {
        let (outcome, cause, mut findings) = match (report.outcome, findings) {
            (gate::GateOutcome::Concluded(conclusion), Some(findings)) => {
                (Settled::Ran(conclusion), report.cause, findings)
            }
            (gate::GateOutcome::Concluded(_), None) => (
                Settled::Error(GateError::InvalidDocument.detail().into_owned()),
                report.cause,
                Vec::new(),
            ),
            (gate::GateOutcome::Error(error), _) => (
                Settled::Error(error.detail().into_owned()),
                report.cause,
                Vec::new(),
            ),
        };
        let fingerprints: Option<Vec<GateFinding>> = findings
            .iter_mut()
            .map(|finding| {
                let fp = anchors.anchor(finding)?;
                Some(GateFinding {
                    fp,
                    // The decision input and the reported finding each own the rule.
                    rule: finding.rule.clone(),
                    severity: finding.severity,
                })
            })
            .collect();
        let (outcome, reported) = match fingerprints {
            Some(reported) => (outcome, reported),
            None => (Settled::Error("evidence-missing".to_owned()), Vec::new()),
        };
        gates.local.push(LocalGate {
            name: report.plugin.as_str().to_owned(),
            job: job_name(report.job),
            required: report.required,
            outcome: outcome.display(),
            cause,
            // The report shows the findings; validation reads them too, below.
            findings: findings.clone(),
        });
        gates.reports.push(GateReport {
            name: report.plugin.as_str().to_owned(),
            required: report.required,
            outcome: outcome.into_domain(),
            findings: reported,
        });
        gates.findings.append(&mut findings);
    }
    gates
}

/// How a gate settled, before it becomes the report's text and the decision's outcome.
enum Settled {
    Ran(Conclusion),
    Error(String),
}

impl Settled {
    fn display(&self) -> String {
        match self {
            Self::Ran(conclusion) => conclusion_code(*conclusion).to_owned(),
            Self::Error(detail) => format!("gate-error:{detail}"),
        }
    }

    fn into_domain(self) -> GateOutcome {
        match self {
            Self::Ran(conclusion) => GateOutcome::Ran {
                conclusion: match conclusion {
                    Conclusion::Pass => GateConclusion::Pass,
                    Conclusion::Fail => GateConclusion::Fail,
                    Conclusion::Neutral => GateConclusion::Neutral,
                },
                steps: StepAttribution::Unknown,
            },
            Self::Error(detail) => GateOutcome::Error { detail },
        }
    }
}

const fn conclusion_code(conclusion: Conclusion) -> &'static str {
    match conclusion {
        Conclusion::Pass => "pass",
        Conclusion::Fail => "fail",
        Conclusion::Neutral => "neutral",
    }
}

const fn job_name(job: GateJob) -> &'static str {
    match job {
        GateJob::Static => "gates-static",
        GateJob::Build(BuildJob::Lint) => "gates-lint",
        GateJob::Build(BuildJob::Test) => "gates-test",
    }
}
