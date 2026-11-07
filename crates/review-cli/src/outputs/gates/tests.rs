use std::assert_matches;

use proptest::prelude::*;

use super::*;
use crate::ci::Outputs;

const TEST: GateJob = GateJob::Build(BuildJob::Test);

/// The sha256 of no bytes.
const EMPTY: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn concluded(plugin: &str, conclusion: GateConclusion, document: &[u8]) -> ReportedGate {
    ReportedGate {
        plugin: plugin.to_owned(),
        reported: Reported::Concluded {
            conclusion,
            document: Sha256Digest::of(document),
        },
    }
}

fn errored(plugin: &str, error: GateError) -> ReportedGate {
    ReportedGate {
        plugin: plugin.to_owned(),
        reported: Reported::Error(error),
    }
}

/// `toJSON(needs)` for one job `job` that ended with `result` and wrote `outputs`.
fn needs_text(job: &str, result: &str, outputs: &[(&str, &str)]) -> String {
    let outputs: Map<String, Value> = outputs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::json!({ job: { "result": result, "outputs": outputs } }).to_string()
}

fn decoded_from(job: GateJob, text: &str) -> Result<Vec<ReportedGate>, NeedsError> {
    let name = job_name(job);
    let needs = Needs::parse(text.as_bytes(), &[name])?;
    gates_from_needs(&needs, job)
}

/// The gates `gates-test` reported as `value`, from a successful job.
fn decoded(value: &str) -> Result<Vec<ReportedGate>, NeedsError> {
    decoded_from(
        TEST,
        &needs_text("gates-test", "success", &[("gates", value)]),
    )
}

#[test]
fn encodes_the_documented_compact_form_in_the_given_order() {
    let gates = [
        concluded("temper", GateConclusion::Pass, b""),
        errored("x", GateError::Timeout),
        concluded("a-1", GateConclusion::Neutral, b""),
    ];

    let encoded = gates_output(&gates);

    assert_eq!(
        encoded,
        Ok(format!(
            "[{{\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{EMPTY}\"}},\
             {{\"plugin\":\"x\",\"error\":\"gate-error:timeout\"}},\
             {{\"plugin\":\"a-1\",\"conclusion\":\"neutral\",\"sha256\":\"{EMPTY}\"}}]"
        ))
    );
}

#[test]
fn no_gates_encode_as_an_empty_list() {
    assert_eq!(gates_output(&[]), Ok("[]".to_owned()));
}

#[test]
fn a_plugin_that_is_not_a_name_is_refused() {
    for plugin in ["", "a\nb", "Temper", "a\"b", "a/../b", "-a"] {
        let gates = [
            concluded("temper", GateConclusion::Pass, b""),
            errored(plugin, GateError::NotRun),
        ];

        assert_eq!(
            gates_output(&gates),
            Err(GatesOutputError::Plugin {
                index: 1,
                name: plugin.to_owned()
            }),
            "{plugin:?}"
        );
    }
}

#[test]
fn a_refused_plugin_is_shown_cut_to_64_bytes() {
    let long = "A".repeat(100);

    let encoded = gates_output(&[errored(&long, GateError::NotRun)]);

    assert_eq!(
        encoded,
        Err(GatesOutputError::Plugin {
            index: 0,
            name: "A".repeat(64)
        })
    );
}

#[test]
fn decodes_the_documented_example() {
    let value = format!(
        "[{{\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{EMPTY}\"}},\
         {{\"plugin\":\"x\",\"error\":\"gate-error:timeout\"}}]"
    );

    let gates = decoded(&value).expect("the documented shape");

    assert_eq!(
        gates,
        [
            concluded("temper", GateConclusion::Pass, b""),
            errored("x", GateError::Timeout),
        ]
    );
}

#[test]
fn an_empty_list_decodes_as_no_gates() {
    assert_eq!(decoded("[]").expect("an empty list"), []);
}

#[test]
fn each_gate_job_reads_its_own_entry() {
    for (job, id) in [
        (GateJob::Static, "gates-static"),
        (GateJob::Build(BuildJob::Lint), "gates-lint"),
        (TEST, "gates-test"),
    ] {
        let value = "[{\"plugin\":\"temper\",\"error\":\"gate-error:skipped\"}]";
        let text = needs_text(id, "success", &[("gates", value)]);

        let gates = decoded_from(job, &text).expect("the job's own entry");

        assert_eq!(gates, [errored("temper", GateError::Skipped)], "{id}");
    }
}

#[test]
fn refuses_each_value_that_is_not_the_documented_shape() {
    let entry = |fields: &str| format!("[{{{fields}}}]");
    let digest = format!("\"sha256\":\"{EMPTY}\"");
    let cases = [
        "{}".to_owned(),
        "null".to_owned(),
        "[1]".to_owned(),
        "[[]]".to_owned(),
        "[{}]".to_owned(),
        "[] []".to_owned(),
        entry("\"plugin\":\"temper\""),
        entry("\"plugin\":\"temper\",\"conclusion\":\"pass\""),
        entry(&format!("\"plugin\":\"temper\",{digest}")),
        entry(&format!("\"conclusion\":\"pass\",{digest}")),
        entry("\"error\":\"gate-error:timeout\""),
        entry(&format!(
            "\"plugin\":\"temper\",\"conclusion\":\"pass\",{digest},\"error\":\"gate-error:timeout\""
        )),
        entry(&format!(
            "\"plugin\":\"temper\",\"conclusion\":\"pass\",{digest},\"extra\":1"
        )),
        entry("\"plugin\":\"temper\",\"error\":\"gate-error:timeout\",\"extra\":1"),
        entry("\"plugin\":\"Temper\",\"error\":\"gate-error:timeout\""),
        entry("\"plugin\":\"\",\"error\":\"gate-error:timeout\""),
        entry("\"plugin\":7,\"error\":\"gate-error:timeout\""),
        entry(&format!(
            "\"plugin\":\"temper\",\"conclusion\":\"Pass\",{digest}"
        )),
        entry(&format!(
            "\"plugin\":\"temper\",\"conclusion\":\"skipped\",{digest}"
        )),
        entry(&format!(
            "\"plugin\":\"temper\",\"conclusion\":true,{digest}"
        )),
        entry("\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"e3b0\""),
        entry(&format!(
            "\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{}\"",
            EMPTY.to_uppercase()
        )),
        entry(&format!(
            "\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{}\"",
            EMPTY.trim_start_matches("sha256:")
        )),
        entry("\"plugin\":\"temper\",\"error\":\"timeout\""),
        entry("\"plugin\":\"temper\",\"error\":\"gate-error:exit-03\""),
        entry("\"plugin\":\"temper\",\"error\":\"gate-error:job-failed\""),
        entry("\"plugin\":\"temper\",\"error\":null"),
        entry("\"plugin\":\"a\",\"plugin\":\"b\",\"error\":\"gate-error:timeout\""),
    ];

    for value in cases {
        assert_matches!(
            decoded(&value),
            Err(NeedsError::Output {
                job: JobName::GatesTest,
                name: OutputName::Gates
            }),
            "{value}"
        );
    }
}

#[test]
fn refuses_a_job_without_its_gates_output() {
    for outputs in [&[][..], &[("gates", "")][..]] {
        let text = needs_text("gates-test", "success", outputs);

        assert_matches!(
            decoded_from(TEST, &text),
            Err(NeedsError::MissingOutput {
                job: JobName::GatesTest,
                name: OutputName::Gates
            }),
            "{outputs:?}"
        );
    }
}

#[test]
fn refuses_an_output_a_gates_job_does_not_write() {
    let text = needs_text("gates-test", "success", &[("gates", "[]"), ("run", "true")]);

    assert_matches!(
        decoded_from(TEST, &text),
        Err(NeedsError::UnexpectedOutput { job: JobName::GatesTest, name }) if name == "run"
    );
}

#[test]
fn refuses_a_gates_job_that_did_not_succeed() {
    for (result, expected) in [
        ("failure", JobResult::Failure),
        ("cancelled", JobResult::Cancelled),
        ("skipped", JobResult::Skipped),
    ] {
        let text = needs_text("gates-test", result, &[("gates", "[]")]);

        assert_matches!(
            decoded_from(TEST, &text),
            Err(NeedsError::NotSucceeded { job: JobName::GatesTest, result }) if result == expected,
            "{result}"
        );
    }
}

#[test]
fn refuses_needs_without_the_gates_job() {
    let needs = Needs::parse(
        needs_text("guard", "success", &[]).as_bytes(),
        &[JobName::Guard],
    )
    .expect("guard's needs");

    assert_matches!(
        gates_from_needs(&needs, TEST),
        Err(NeedsError::MissingJob {
            job: JobName::GatesTest
        })
    );
}

/// Any gate error, an exit code of any value included.
fn gate_errors() -> impl Strategy<Value = GateError> {
    prop_oneof![
        any::<i32>().prop_map(GateError::Exit),
        Just(GateError::Signal),
        Just(GateError::Timeout),
        Just(GateError::OutputTooLarge),
        Just(GateError::MissingDocument),
        Just(GateError::DocumentTooLarge),
        Just(GateError::InvalidDocument),
        Just(GateError::NotRun),
        Just(GateError::Skipped),
        Just(GateError::RatchetDegraded),
    ]
}

fn reported_gates() -> impl Strategy<Value = ReportedGate> {
    let reported = prop_oneof![
        (
            proptest::sample::select(GateConclusion::ALL.to_vec()),
            proptest::collection::vec(any::<u8>(), 0..32),
        )
            .prop_map(|(conclusion, bytes)| Reported::Concluded {
                conclusion,
                document: Sha256Digest::of(&bytes),
            }),
        gate_errors().prop_map(Reported::Error),
    ];
    ("[a-z0-9][a-z0-9-]{0,63}", reported)
        .prop_map(|(plugin, reported)| ReportedGate { plugin, reported })
}

/// The gates a consumer of `gates-test` reads after `gates` went through the `GITHUB_OUTPUT`
/// writer.
fn through_outputs(gates: &[ReportedGate]) -> Result<Vec<ReportedGate>, Box<dyn Error>> {
    let value = gates_output(gates)?;
    let mut file = Vec::new();
    Outputs::new(&mut file).write(OutputName::Gates, &value)?;
    let text = String::from_utf8(file)?;
    let (name, value) = text
        .strip_suffix('\n')
        .and_then(|line| line.split_once('='))
        .ok_or("one name=value line")?;
    Ok(decoded_from(
        TEST,
        &needs_text("gates-test", "success", &[(name, value)]),
    )?)
}

proptest! {
    #[test]
    fn decode_inverts_encode(gates in proptest::collection::vec(reported_gates(), 0..8)) {
        let read = through_outputs(&gates);

        prop_assert_eq!(read.ok(), Some(gates));
    }
}
