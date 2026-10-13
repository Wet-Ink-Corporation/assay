//! Local mode's gates: every plugin the base schedules, run job by job through the local
//! `GateRunner`, with their findings anchored on the snapshot as `decide` anchors them.

use assay_domain::{Finding, FindingsDocument, GateFinding, GateReport, SourceKind};
use serde::Serialize;

use super::events::{EventFailure, GateEvents, Recorder};
use super::outcome::{GateErrorText, LocalOutcome};
use super::report::LocalFinding;
use crate::config::PluginsConfig;
use crate::contract::ContractValidator;
use crate::decide::{Anchors, Cited, Texts};
use crate::events::EventSink;
use crate::gate::{
    self, BuildJob, GateError, GateJob, GateObserver, GateRunner, account_for_required,
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
    pub outcome: LocalOutcome,
    /// Why it is a gate error, in assay's words (`not run in local mode`); never gate output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// Its findings, anchored on the snapshot and fingerprinted: a verified one quotes the
    /// tree's line. None when one of them could not be anchored (`evidence-missing`).
    pub findings: Vec<LocalFinding>,
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

/// Runs every plugin `config` schedules in each job, in job then file order, recording each
/// gate's start and end. A required gate the runner does not run is a gate error, never a pass,
/// and ends without having started.
pub(super) fn run<G: GateRunner, S: EventSink>(
    config: &PluginsConfig,
    runner: &mut G,
    validator: &ContractValidator,
    recorder: &mut Recorder<'_, S>,
) -> Result<Vec<Ran>, EventFailure<S::Error>> {
    let mut ran = Vec::new();
    for job in JOBS {
        let mut observer = GateEvents::new(recorder, job);
        let mut reports = gate::run_observed(config, runner, validator, &mut observer)?;
        account_and_record(config, &mut reports, &mut observer)?;
        ran.extend(reports.into_iter().map(read));
    }
    Ok(ran)
}

/// Appends a `gate-error:skipped` report for each required gate of the observer's job that
/// `reports` lacks, and records its end (it never started). `run_observed` reports every gate
/// it schedules, so this finds none today; it keeps a missing required gate from reading as a
/// pass, as in CI.
fn account_and_record<O: GateObserver>(
    config: &PluginsConfig,
    reports: &mut Vec<gate::GateReport>,
    observer: &mut O,
) -> Result<(), O::Error> {
    let ran_count = reports.len();
    account_for_required(config, observer.job(), reports);
    reports
        .iter()
        .skip(ran_count)
        .try_for_each(|report| observer.ended(report, None))
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
        let (outcome, mut findings) = match (report.outcome, findings) {
            (gate::GateOutcome::Concluded(conclusion), Some(findings)) => {
                (LocalOutcome::concluded(conclusion), findings)
            }
            (gate::GateOutcome::Concluded(_), None) => {
                (gate_error(&GateError::InvalidDocument.detail()), Vec::new())
            }
            (gate::GateOutcome::Error(error), _) => (gate_error(&error.detail()), Vec::new()),
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
            None => (gate_error("evidence-missing"), Vec::new()),
        };
        let shown = reported
            .iter()
            .zip(&findings)
            .map(|(reported, finding)| LocalFinding {
                fp: reported.fp,
                // The report shows the finding; validation reads it too, below.
                finding: finding.clone(),
            })
            .collect();
        gates.reports.push(GateReport {
            name: report.plugin.as_str().to_owned(),
            required: report.required,
            outcome: outcome.to_domain(),
            findings: reported,
        });
        gates.local.push(LocalGate {
            name: report.plugin.as_str().to_owned(),
            job: job_name(report.job),
            required: report.required,
            outcome,
            cause: report.cause,
            findings: shown,
        });
        gates.findings.append(&mut findings);
    }
    gates
}

/// `gate-error:<detail>`.
fn gate_error(detail: &str) -> LocalOutcome {
    LocalOutcome::GateError(GateErrorText::of(detail))
}

const fn job_name(job: GateJob) -> &'static str {
    match job {
        GateJob::Static => "gates-static",
        GateJob::Build(BuildJob::Lint) => "gates-lint",
        GateJob::Build(BuildJob::Test) => "gates-test",
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use crate::config::{Plugin, PluginsConfig};
    use crate::gate::{BuildJob, GateError, GateJob, GateObserver, GateOutcome, GateReport};

    use super::account_and_record;

    /// One required `exec` gate, `unit`, in the test job.
    const CONFIG: &str = r#"
[assay]
digest = "sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b"
protected_bases = ["main"]

[[plugin]]
name = "unit"
adapter = "exec"
exec = ["./scripts/test.sh"]
job = "test"
required = true
"#;

    /// Keeps each ended gate's plugin, outcome and exit.
    #[derive(Default)]
    struct Ended(Vec<(String, GateOutcome, Option<i32>)>);

    impl GateObserver for Ended {
        type Error = Infallible;

        fn job(&self) -> GateJob {
            GateJob::Build(BuildJob::Test)
        }

        fn started(&mut self, _plugin: &Plugin) -> Result<(), Infallible> {
            Ok(())
        }

        fn ended(&mut self, report: &GateReport, exit: Option<i32>) -> Result<(), Infallible> {
            // The fake keeps its own copy of the plugin's name.
            let seen = (report.plugin.as_str().to_owned(), report.outcome, exit);
            self.0.push(seen);
            Ok(())
        }
    }

    #[test]
    fn a_required_gate_with_no_report_is_skipped_and_its_end_recorded()
    -> Result<(), Box<dyn std::error::Error>> {
        let config: PluginsConfig = CONFIG.parse()?;
        let mut reports = Vec::new();
        let mut observer = Ended::default();

        let Ok(()) = account_and_record(&config, &mut reports, &mut observer);

        assert_eq!(
            observer.0,
            [(
                "unit".to_owned(),
                GateOutcome::Error(GateError::Skipped),
                None
            )]
        );
        assert_eq!(reports.len(), 1);
        Ok(())
    }

    #[test]
    fn a_required_gate_that_reported_is_not_recorded_again()
    -> Result<(), Box<dyn std::error::Error>> {
        let config: PluginsConfig = CONFIG.parse()?;
        let mut reports = Vec::new();
        let mut first = Ended::default();
        let Ok(()) = account_and_record(&config, &mut reports, &mut first);
        let mut observer = Ended::default();

        let Ok(()) = account_and_record(&config, &mut reports, &mut observer);

        assert_eq!(observer.0, []);
        assert_eq!(reports.len(), 1);
        Ok(())
    }
}
