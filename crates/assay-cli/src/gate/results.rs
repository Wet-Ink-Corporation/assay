//! What a gates job leaves for the jobs after it: each concluded gate's document as
//! `<out>/<plugin>.json`, and the `gates` output (03 §4a) naming each document by the sha256
//! of its exact bytes. Everything is encoded before anything is written, and the output is
//! appended last, so a job that cannot encode or write its documents appends nothing.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use assay_app::{GateJobOutcome, GateJobReport, Reported, ReportedGate, Sha256Digest};
use assay_cli::outputs::gates_output;
use assay_cli::{OutputName, Outputs};
use tempfile::NamedTempFile;

use super::GateCliError;

/// A concluded gate's document, encoded as it is written.
#[derive(Debug, PartialEq, Eq)]
struct Document<'r> {
    /// The plugin; the file is `<plugin>.json`.
    plugin: &'r str,
    /// The file's bytes.
    bytes: Vec<u8>,
}

/// Creates `out`, writes each concluded gate's document there as `<plugin>.json`, then appends
/// the `gates` output for `reports`, in their order, to `output`.
///
/// # Errors
///
/// [`GateCliError::NoDocument`], [`GateCliError::Encode`] or [`GateCliError::GatesOutput`]
/// when something cannot be encoded, with nothing written; [`GateCliError::OutExists`] or
/// [`GateCliError::Write`] when a document cannot be written, with no output appended; and
/// [`GateCliError::Output`] when the output cannot be appended.
pub(super) fn write(
    out: &Path,
    reports: &[GateJobReport],
    output: impl Write,
) -> Result<(), GateCliError> {
    let (documents, gates) = encode(reports)?;
    let gates = gates_output(&gates).map_err(GateCliError::GatesOutput)?;
    write_documents(out, &documents)?;
    Outputs::new(output)
        .write(OutputName::Gates, &gates)
        .map_err(GateCliError::Output)
}

/// Each concluded gate's document as the bytes to write, and what the job reports for every
/// gate, in the order of `reports`.
fn encode(
    reports: &[GateJobReport],
) -> Result<(Vec<Document<'_>>, Vec<ReportedGate>), GateCliError> {
    let mut documents = Vec::new();
    let mut gates = Vec::with_capacity(reports.len());
    for report in reports {
        let plugin = report.plugin.as_str();
        let reported = match report.outcome {
            GateJobOutcome::Concluded(conclusion) => {
                let document = report
                    .document
                    .as_ref()
                    .ok_or_else(|| GateCliError::NoDocument(plugin.to_owned()))?;
                let bytes = serde_json::to_vec(document).map_err(GateCliError::Encode)?;
                let digest = Sha256Digest::of(&bytes);
                documents.push(Document { plugin, bytes });
                Reported::Concluded {
                    conclusion,
                    document: digest,
                }
            }
            GateJobOutcome::Error(error) => Reported::Error(error),
        };
        // The report keeps its name; the output's list owns its own.
        gates.push(ReportedGate {
            plugin: plugin.to_owned(),
            reported,
        });
    }
    Ok((documents, gates))
}

/// Creates `out` and writes each document there as `<plugin>.json`.
fn write_documents(out: &Path, documents: &[Document<'_>]) -> Result<(), GateCliError> {
    let failed = |path: &Path| {
        let path = path.to_path_buf();
        move |source| GateCliError::Write { path, source }
    };
    fs::create_dir(out).map_err(|source| match source.kind() {
        io::ErrorKind::AlreadyExists => GateCliError::OutExists(out.to_path_buf()),
        _ => failed(out)(source),
    })?;
    for Document { plugin, bytes } in documents {
        // A plugin name is `[a-z0-9-]`, so it is one path segment.
        let path = out.join(format!("{plugin}.json"));
        let mut file = NamedTempFile::new_in(out).map_err(failed(out))?;
        file.write_all(bytes).map_err(failed(&path))?;
        file.persist_noclobber(&path)
            .map_err(|error| failed(&path)(error.error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use assay_app::{GateError, GateName};
    use assay_domain::GateConclusion;
    use serde_json::json;

    use super::super::TEST;
    use super::*;

    /// The sha256 of `{"a":1}`.
    const A1: &str = "sha256:015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862";

    fn name(plugin: &str) -> GateName {
        plugin.parse().expect("a plugin name")
    }

    fn concluded(plugin: &str, document: Option<serde_json::Value>) -> GateJobReport {
        GateJobReport {
            plugin: name(plugin),
            job: TEST,
            required: true,
            outcome: GateJobOutcome::Concluded(GateConclusion::Pass),
            document,
            cause: None,
        }
    }

    fn errored(plugin: &str, error: GateError) -> GateJobReport {
        GateJobReport {
            plugin: name(plugin),
            job: TEST,
            required: false,
            outcome: GateJobOutcome::Error(error),
            document: None,
            cause: Some("why".to_owned()),
        }
    }

    #[test]
    fn a_concluded_gate_is_reported_by_the_sha256_of_its_compact_document() {
        let reports = [
            errored("x", GateError::Timeout),
            concluded("temper", Some(json!({ "a": 1 }))),
        ];

        let (documents, gates) = encode(&reports).expect("encodable reports");

        assert_eq!(
            documents,
            [Document {
                plugin: "temper",
                bytes: b"{\"a\":1}".to_vec()
            }]
        );
        assert_eq!(
            gates,
            [
                ReportedGate {
                    plugin: "x".to_owned(),
                    reported: Reported::Error(GateError::Timeout),
                },
                ReportedGate {
                    plugin: "temper".to_owned(),
                    reported: Reported::Concluded {
                        conclusion: GateConclusion::Pass,
                        document: A1.parse().expect("a digest"),
                    },
                },
            ]
        );
    }

    #[test]
    fn the_documents_are_written_and_then_the_output_is_appended() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let out = dir.path().join("out");
        let mut output = b"earlier=1\n".to_vec();

        write(
            &out,
            &[
                concluded("temper", Some(json!({ "a": 1 }))),
                errored("x", GateError::NotRun),
            ],
            &mut output,
        )
        .expect("written");

        assert_eq!(
            fs::read(out.join("temper.json")).expect("temper's document"),
            b"{\"a\":1}"
        );
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            format!(
                "earlier=1\ngates=[{{\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{A1}\"}},\
                 {{\"plugin\":\"x\",\"error\":\"gate-error:not-run\"}}]\n"
            )
        );
    }

    #[test]
    fn a_report_that_cannot_be_encoded_writes_nothing() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let out = dir.path().join("out");
        let mut output = Vec::new();

        let written = write(
            &out,
            &[
                concluded("temper", Some(json!({ "a": 1 }))),
                concluded("probe", None),
            ],
            &mut output,
        );

        assert_matches!(written, Err(GateCliError::NoDocument(plugin)) if plugin == "probe");
        assert!(!out.exists(), "no document is written");
        assert_eq!(output, b"", "no output is appended");
    }

    #[test]
    fn documents_that_cannot_be_written_append_no_output() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let out = dir.path().join("out");
        fs::create_dir(&out).expect("made first");
        let mut output = Vec::new();

        let written = write(
            &out,
            &[concluded("temper", Some(json!({ "a": 1 })))],
            &mut output,
        );

        assert_matches!(written, Err(GateCliError::OutExists(path)) if path == out);
        assert_eq!(output, b"", "no output is appended");
    }
}
