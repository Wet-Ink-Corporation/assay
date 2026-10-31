//! A gates job's `gates` output (03 §4a): what it reported for each plugin it ran, in file
//! order, as compact JSON. `{"plugin", "conclusion", "sha256"}` for a gate that concluded,
//! with the sha256 of the exact bytes of the `<plugin>.json` it uploaded, and
//! `{"plugin", "error"}` for a gate error, written as its reason (`gate-error:<detail>`).

use std::error::Error;
use std::fmt;

use assay_app::{
    BuildJob, GateError, GateJob, GateName, ParseGateNameError, Reported, ReportedGate,
    Sha256Digest,
};
use assay_domain::GateConclusion;
use serde_json::{Map, Value};

use crate::ci::{JobName, JobResult, Needs, NeedsError, OutputName, shown};

/// Why a gates job's reports could not be encoded as its `gates` output; nothing is encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatesOutputError {
    /// A report's plugin is not a plugin name (1 to 64 of `a-z`, `0-9` and `-`, starting with
    /// a letter or digit).
    Plugin {
        /// The report's position in the list.
        index: usize,
        /// The plugin as given, its first 64 bytes.
        name: String,
    },
}

impl fmt::Display for GatesOutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plugin { index, name } => write!(
                f,
                "gate report {index} names plugin {name:?}, which is not a plugin name; the \
                 gates output was not written"
            ),
        }
    }
}

impl Error for GatesOutputError {}

/// The `gates` output's value for `gates`, in the order given: one compact JSON array, with
/// `plugin` first in each entry.
///
/// ```
/// use assay_app::{GateError, Reported, ReportedGate};
/// use assay_cli::outputs::gates_output;
///
/// let gates = [ReportedGate {
///     plugin: "x".to_owned(),
///     reported: Reported::Error(GateError::Timeout),
/// }];
/// assert_eq!(
///     gates_output(&gates)?,
///     r#"[{"plugin":"x","error":"gate-error:timeout"}]"#
/// );
/// # Ok::<(), assay_cli::outputs::GatesOutputError>(())
/// ```
///
/// # Errors
///
/// [`GatesOutputError::Plugin`] for the first report whose plugin is not a plugin name.
pub fn gates_output(gates: &[ReportedGate]) -> Result<String, GatesOutputError> {
    let entries = gates
        .iter()
        .enumerate()
        .map(|(index, gate)| entry_text(index, gate))
        .collect::<Result<Vec<String>, GatesOutputError>>()?;
    Ok(format!("[{}]", entries.join(",")))
}

/// One report as its JSON object.
fn entry_text(index: usize, gate: &ReportedGate) -> Result<String, GatesOutputError> {
    gate.plugin
        .parse::<GateName>()
        .map_err(|ParseGateNameError| GatesOutputError::Plugin {
            index,
            name: shown(&gate.plugin),
        })?;
    let plugin = json(&gate.plugin);
    Ok(match &gate.reported {
        Reported::Concluded {
            conclusion,
            document,
        } => format!(
            "{{\"plugin\":{plugin},\"conclusion\":{},\"sha256\":{}}}",
            json(conclusion.code()),
            json(document.as_str())
        ),
        Reported::Error(error) => format!(
            "{{\"plugin\":{plugin},\"error\":{}}}",
            json(&error.to_string())
        ),
    })
}

/// `text` as a JSON string.
fn json(text: &str) -> String {
    Value::from(text).to_string()
}

/// The workflow job of gate job `job`.
const fn job_name(job: GateJob) -> JobName {
    match job {
        GateJob::Static => JobName::GatesStatic,
        GateJob::Build(BuildJob::Lint) => JobName::GatesLint,
        GateJob::Build(BuildJob::Test) => JobName::GatesTest,
    }
}

/// What gates job `job` reported, read from its `gates` output in `needs`, in its order.
///
/// Only a job that succeeded is read: the reports of any other are not believed, and its
/// gates are the reader's to account for. A plugin named twice or not scheduled is left to
/// `decide`, which judges the list against base config.
///
/// # Errors
///
/// [`NeedsError::MissingJob`] without the job; [`NeedsError::NotSucceeded`] when it did not
/// succeed; [`NeedsError::UnexpectedOutput`] for an output other than `gates`;
/// [`NeedsError::MissingOutput`] when `gates` is absent or empty; and [`NeedsError::Output`]
/// when it is not exactly the encoded shape: one JSON array without repeated keys whose every
/// entry is `{plugin, conclusion, sha256}` or `{plugin, error}`, with a plugin name, a
/// conclusion `pass`, `fail` or `neutral`, a `sha256:<64 lowercase hex>` digest and a gate
/// error's reason exactly as assay writes it.
pub fn gates_from_needs(needs: &Needs, job: GateJob) -> Result<Vec<ReportedGate>, NeedsError> {
    let job = job_name(job);
    let upstream = needs.job(job).ok_or(NeedsError::MissingJob { job })?;
    if upstream.result() != JobResult::Success {
        return Err(NeedsError::NotSucceeded {
            job,
            result: upstream.result(),
        });
    }
    if let Some((name, _)) = upstream
        .outputs()
        .find(|&(name, _)| !OutputName::GATES.iter().any(|own| own.as_str() == name))
    {
        return Err(NeedsError::UnexpectedOutput {
            job,
            name: shown(name),
        });
    }
    let value = upstream
        .output(OutputName::Gates.as_str())
        .filter(|value| !value.is_empty())
        .ok_or(NeedsError::MissingOutput {
            job,
            name: OutputName::Gates,
        })?;
    reports(value).ok_or(NeedsError::Output {
        job,
        name: OutputName::Gates,
    })
}

/// The reports `text` encodes, if it is exactly the encoded shape.
fn reports(text: &str) -> Option<Vec<ReportedGate>> {
    let Value::Array(entries) = assay_app::parse_json_strict(text.as_bytes()).ok()? else {
        return None;
    };
    entries.into_iter().map(report).collect()
}

/// One report from its JSON object.
fn report(value: Value) -> Option<ReportedGate> {
    let Value::Object(mut fields) = value else {
        return None;
    };
    let plugin = string(&mut fields, "plugin")?;
    plugin.parse::<GateName>().ok()?;
    let reported = if fields.contains_key("error") {
        Reported::Error(string(&mut fields, "error")?.parse::<GateError>().ok()?)
    } else {
        let conclusion = string(&mut fields, "conclusion")?;
        Reported::Concluded {
            conclusion: GateConclusion::ALL
                .into_iter()
                .find(|known| known.code() == conclusion)?,
            document: string(&mut fields, "sha256")?
                .parse::<Sha256Digest>()
                .ok()?,
        }
    };
    fields
        .is_empty()
        .then_some(ReportedGate { plugin, reported })
}

/// Field `key` of `fields`, taken out, when it is a string.
fn string(fields: &mut Map<String, Value>, key: &str) -> Option<String> {
    match fields.remove(key)? {
        Value::String(text) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
