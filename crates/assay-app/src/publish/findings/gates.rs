//! The non-required per-plugin checks `assay / gate: <name>` (04 §6): each gate's conclusion
//! and its findings, beside the one required verdict check.

use std::iter;

use assay_domain::{GateConclusion, GateOutcome, GateReport, Severity};

use crate::config::GateName;
use crate::publish::ports::{CheckConclusion, CheckName, CheckState, CheckUpdate};
use crate::publish::render;

/// One gate check to write.
pub(super) struct GateCheck {
    /// `assay / gate: <name>`.
    pub(super) name: CheckName,
    /// What it shows.
    pub(super) update: CheckUpdate,
}

/// The check of every gate in `gates`, keyed by `decision_key`, or the first gate name that is
/// not a plugin name (base config admits no other, so this is a defect upstream).
pub(super) fn gate_checks(
    gates: &[GateReport],
    decision_key: &str,
) -> Result<Vec<GateCheck>, String> {
    gates
        .iter()
        .map(|gate| {
            // The validated name owns its copy of the plugin name.
            let name = GateName::new(gate.name.clone())?;
            Ok(GateCheck {
                update: update(&name, gate, decision_key),
                name: CheckName::for_gate(&name),
            })
        })
        .collect()
}

/// The check update for `gate`: `success`, `failure` or `neutral` as the gate concluded, and
/// `failure` for a gate that errored or was skipped, which is never a pass.
fn update(name: &GateName, gate: &GateReport, decision_key: &str) -> CheckUpdate {
    let (conclusion, word) = match &gate.outcome {
        GateOutcome::Ran { conclusion, .. } => (
            match conclusion {
                GateConclusion::Pass => CheckConclusion::Success,
                GateConclusion::Fail => CheckConclusion::Failure,
                GateConclusion::Neutral => CheckConclusion::Neutral,
            },
            conclusion.code(),
        ),
        GateOutcome::Error { .. } => (CheckConclusion::Failure, "error"),
        GateOutcome::Skipped => (CheckConclusion::Failure, "skipped"),
    };
    CheckUpdate {
        state: CheckState::Completed(conclusion),
        title: format!("assay gate {name}: {word}"),
        summary: summary(gate),
        text: ["```text\ndecision_key: ", decision_key, "\n```"].concat(),
        // The check owns its external id.
        external_id: decision_key.to_owned(),
    }
}

/// The gate's error or skip, its finding counts and its findings, each in a fenced block.
fn summary(gate: &GateReport) -> String {
    let mut lines = Vec::new();
    match &gate.outcome {
        GateOutcome::Ran { .. } => {}
        GateOutcome::Error { detail } => {
            lines.push("The gate errored:".to_owned());
            // The block renders an owned copy of the detail.
            render::block(&mut lines, 1, iter::once(detail.clone()));
            lines.push(String::new());
        }
        GateOutcome::Skipped => {
            lines.push("The gate did not run.".to_owned());
            lines.push(String::new());
        }
    }
    let count = |severity: Severity| {
        gate.findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    lines.push(format!(
        "Findings: {} blocker · {} major · {} minor · {} nit",
        count(Severity::Blocker),
        count(Severity::Major),
        count(Severity::Minor),
        count(Severity::Nit),
    ));
    lines.push(String::new());
    let items = gate.findings.iter().map(|finding| {
        let severity = finding.severity.code();
        match &finding.rule {
            Some(rule) => format!("{severity} {rule} ({})", finding.fp),
            None => format!("{severity} ({})", finding.fp),
        }
    });
    render::block(&mut lines, gate.findings.len(), items);
    lines.join("\n")
}
