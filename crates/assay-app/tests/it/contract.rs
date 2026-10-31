//! Validate-on-read for the versioned contracts (`docs/architecture/03-contracts.md` preamble,
//! §3 to §6) against the golden fixtures in `schemas/fixtures/`.

use std::assert_matches;
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;

use assay_app::{Contract, ContractValidator, ReadContractError};
use assay_domain::{CallFailure, DecisionInputs, DegradedReport, Disposition, StageRole, decide};
use serde_json::Value;

use crate::support::{Validity, fixtures, golden_inputs, rubric_v1, schemas_dir};

/// How each invalid fixture must be rejected: `Version(n)` for an unknown version, otherwise the
/// JSON Pointer of the offending instance and the schema keyword that rejects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejection<'a> {
    Version(u32),
    Violation(&'a str, &'a str),
}

const EXPECTED_REJECTIONS: [(Contract, &str, Rejection<'static>); 61] = [
    (
        Contract::Status,
        "unknown-field",
        Rejection::Violation("", "additionalProperties"),
    ),
    (Contract::Status, "unknown-version", Rejection::Version(2)),
    // `ci` or `local` (#130), nothing else.
    (
        Contract::Status,
        "unknown-source",
        Rejection::Violation("/source", "enum"),
    ),
    // One Weigh-In state: the retired booleans cannot sit beside it.
    (
        Contract::Status,
        "weigh-in-two-claims",
        Rejection::Violation("/weigh_in", "additionalProperties"),
    ),
    (
        Contract::Status,
        "weigh-in-unknown-state",
        Rejection::Violation("/weigh_in/state", "enum"),
    ),
    (
        Contract::Status,
        "uppercase-fp",
        Rejection::Violation("/findings/0/fp", "pattern"),
    ),
    (
        Contract::Status,
        "over-length-title",
        Rejection::Violation("/findings/0/title", "oneOf"),
    ),
    // Only a github.com page of the repository is a link the mod may open.
    (
        Contract::Status,
        "off-site-check-url",
        Rejection::Violation("/check", "oneOf"),
    ),
    (
        Contract::Status,
        "finding-without-waivable",
        Rejection::Violation("/findings/0", "required"),
    ),
    (
        Contract::Status,
        "viewer-login-not-a-login",
        Rejection::Violation("/viewer/login", "pattern"),
    ),
    // A local status (#130) has no viewer, no check page and nothing waivable, and a CI status
    // carries no local tree.
    (
        Contract::Status,
        "local-with-viewer",
        Rejection::Violation("/viewer", "falseSchema"),
    ),
    (
        Contract::Status,
        "local-check-with-url",
        Rejection::Violation("/check", "oneOf"),
    ),
    (
        Contract::Status,
        "local-waivable-finding",
        Rejection::Violation("/findings/0/waivable", "const"),
    ),
    // Stale only against a local result: with no `tree` there is nothing to be stale.
    (
        Contract::Status,
        "local-stale-without-result",
        Rejection::Violation("/stale", "const"),
    ),
    (
        Contract::Status,
        "ci-with-local-tree",
        Rejection::Violation("/local_tree", "falseSchema"),
    ),
    // A refusal (#218) is the App verdict check's reason code, from 03's closed list, on a
    // completed `failure` check; local mode has no App check to have refused it.
    (
        Contract::Status,
        "local-refused",
        Rejection::Violation("/refused", "falseSchema"),
    ),
    (
        Contract::Status,
        "refused-unknown-reason",
        Rejection::Violation("/refused/reason", "enum"),
    ),
    (
        Contract::Status,
        "refused-check-not-failed",
        Rejection::Violation("/check/conclusion", "const"),
    ),
    (
        Contract::Findings,
        "reviewer-without-role",
        Rejection::Violation("/source", "required"),
    ),
    (
        Contract::Findings,
        "producer-group-id",
        Rejection::Violation("/findings/0", "additionalProperties"),
    ),
    (
        Contract::Findings,
        "over-length-caller-evidence",
        Rejection::Violation("/findings/1/caller/evidence", "maxLength"),
    ),
    (
        Contract::Run,
        "missing-run-uid",
        Rejection::Violation("", "required"),
    ),
    (
        Contract::Run,
        "unknown-deferral",
        Rejection::Violation("/budget/deferral", "enum"),
    ),
    // Only a `full` run degrades, and it degrades to `lite` (04 §2.4).
    (
        Contract::Run,
        "degraded-tier-not-lite",
        Rejection::Violation("/tier", "const"),
    ),
    (
        Contract::Run,
        "budget-without-deferral",
        Rejection::Violation("/budget", "required"),
    ),
    (
        Contract::Run,
        "retry-grant-unknown-field",
        Rejection::Violation("/budget/retry_grant", "additionalProperties"),
    ),
    (
        Contract::Run,
        "reused-evidence-unknown-llm",
        Rejection::Violation("/budget/reused_evidence/llm", "enum"),
    ),
    // A built-in gate has a pin and an `exec` gate a job, never both (03 §4).
    (
        Contract::Run,
        "plugin-with-pin-and-job",
        Rejection::Violation("/plugins/0", "oneOf"),
    ),
    // The key under the open `artifact_names` map is producer-chosen: the path masks it.
    (
        Contract::Run,
        "empty-artifact-name",
        Rejection::Violation("/artifact_names/*", "minLength"),
    ),
    (
        Contract::Event,
        "missing-run-uid",
        Rejection::Violation("", "required"),
    ),
    (
        Contract::Event,
        "run-uid-unknown-field",
        Rejection::Violation("/run_uid", "additionalProperties"),
    ),
    (
        Contract::Event,
        "push-recorded-unknown-data-field",
        Rejection::Violation("/data", "additionalProperties"),
    ),
    (
        Contract::Event,
        "preflight-builtins-unknown-field",
        Rejection::Violation("/data/builtins", "additionalProperties"),
    ),
    (
        Contract::Event,
        "command-voided-unknown-reason",
        Rejection::Violation("/data/reason", "enum"),
    ),
    (
        Contract::Event,
        "slot-reaped-without-run-uid",
        Rejection::Violation("/data", "required"),
    ),
    // #170: a disposition recorded after the evidence was located names the finding's `fp`.
    (
        Contract::Event,
        "finding-disposed-moved-without-fp",
        Rejection::Violation("/data", "required"),
    ),
    (
        Contract::Event,
        "finding-disposed-ceiling-without-fp",
        Rejection::Violation("/data", "required"),
    ),
    // #263: so do the coordinator's `duplicate` and the verifier's downgrade.
    (
        Contract::Event,
        "finding-disposed-duplicate-without-fp",
        Rejection::Violation("/data", "required"),
    ),
    (
        Contract::Event,
        "finding-disposed-downgraded-without-fp",
        Rejection::Violation("/data", "required"),
    ),
    (
        Contract::Findings,
        "unknown-field",
        Rejection::Violation("/findings/0", "additionalProperties"),
    ),
    (Contract::Findings, "unknown-version", Rejection::Version(2)),
    (
        Contract::Findings,
        "over-length-evidence",
        Rejection::Violation("/findings/0/evidence", "maxLength"),
    ),
    (
        Contract::Findings,
        "over-length-trigger",
        Rejection::Violation("/findings/0/trigger", "maxLength"),
    ),
    (
        Contract::Findings,
        "over-length-title",
        Rejection::Violation("/findings/0/title", "maxLength"),
    ),
    (
        Contract::Findings,
        "over-length-body",
        Rejection::Violation("/findings/0/body", "maxLength"),
    ),
    (
        Contract::Findings,
        "over-length-fix",
        Rejection::Violation("/findings/0/fix", "maxLength"),
    ),
    (
        Contract::Findings,
        "over-length-expected-behavior",
        Rejection::Violation("/findings/1/expected_behavior", "maxLength"),
    ),
    (
        Contract::Findings,
        "reviewer-conclusion",
        Rejection::Violation("", "not"),
    ),
    (
        Contract::Findings,
        "gate-without-conclusion",
        Rejection::Violation("", "required"),
    ),
    (
        Contract::Findings,
        "producer-fp",
        Rejection::Violation("/findings/0", "additionalProperties"),
    ),
    (
        Contract::Findings,
        "omission-without-expected-behavior",
        Rejection::Violation("/findings/1", "required"),
    ),
    (
        Contract::Findings,
        "interaction-without-caller",
        Rejection::Violation("/findings/1", "required"),
    ),
    (
        Contract::Run,
        "unknown-field",
        Rejection::Violation("", "additionalProperties"),
    ),
    (Contract::Run, "unknown-version", Rejection::Version(2)),
    (
        Contract::Decision,
        "unknown-field",
        Rejection::Violation("/counts", "additionalProperties"),
    ),
    (Contract::Decision, "unknown-version", Rejection::Version(7)),
    (
        Contract::Event,
        "unknown-field",
        Rejection::Violation("", "additionalProperties"),
    ),
    (
        Contract::Event,
        "unknown-data-field",
        Rejection::Violation("/data", "additionalProperties"),
    ),
    (Contract::Event, "unknown-version", Rejection::Version(2)),
    (
        Contract::Event,
        "waiver-applied-uppercase-fp",
        Rejection::Violation("/data/fp", "pattern"),
    ),
    // `data` is an object even for the kinds whose fields 03 §6 does not list.
    (
        Contract::Event,
        "data-not-an-object",
        Rejection::Violation("/data", "type"),
    ),
];

fn rejection_of(error: &ReadContractError) -> Option<Rejection<'_>> {
    match error {
        ReadContractError::UnknownVersion { version, .. } => Some(Rejection::Version(*version)),
        ReadContractError::Invalid { violation, .. } => Some(Rejection::Violation(
            violation.instance_path(),
            violation.keyword(),
        )),
        _ => None,
    }
}

#[test]
fn validator_compiles_every_embedded_schema() {
    let validator = ContractValidator::new();

    assert_matches!(validator, Ok(_));
}

#[test]
fn every_valid_fixture_reads_back_as_its_json() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    for contract in Contract::ALL {
        let valid = fixtures(contract, Validity::Valid)?;
        assert!(!valid.is_empty(), "{contract} has no valid fixture");
        for (name, bytes) in valid {
            let expected: Value = serde_json::from_slice(&bytes)?;

            let read = validator.read(contract, &bytes);

            assert_eq!(
                read.as_ref().ok(),
                Some(&expected),
                "{contract} fixture {name} must be valid, got {read:?}"
            );
        }
    }
    Ok(())
}

/// #147: `data` stays an open object for the event kinds 03 §6 lists no fields for, so the
/// schema accepts what the domain refuses (`assay-domain`'s `round_trip.rs`).
#[test]
fn the_event_schema_accepts_data_on_a_field_less_kind() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let accepted = fixtures(Contract::Event, Validity::DomainInvalid)?;

    let names: Vec<&str> = accepted.keys().map(String::as_str).collect();

    assert_eq!(names, ["run-requested-with-data"]);
    for (name, bytes) in accepted {
        let read = validator.read(Contract::Event, &bytes);

        assert!(read.is_ok(), "event fixture {name}: {read:?}");
    }
    Ok(())
}

#[test]
fn every_invalid_fixture_is_rejected_as_expected() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    for (contract, name, expected) in EXPECTED_REJECTIONS {
        let invalid = fixtures(contract, Validity::Invalid)?;
        let bytes = invalid
            .get(name)
            .ok_or_else(|| format!("{contract} has no invalid fixture {name}"))?;

        let rejection = validator.read(contract, bytes).err();

        assert_eq!(
            rejection.as_ref().and_then(rejection_of),
            Some(expected),
            "{contract} fixture {name} was rejected as {rejection:?}"
        );
    }
    Ok(())
}

#[test]
fn every_invalid_fixture_has_an_expected_rejection() -> Result<(), Box<dyn Error>> {
    let mut on_disk = BTreeSet::new();
    for contract in Contract::ALL {
        for name in fixtures(contract, Validity::Invalid)?.into_keys() {
            on_disk.insert((contract.to_string(), name));
        }
    }

    let listed: BTreeSet<(String, String)> = EXPECTED_REJECTIONS
        .iter()
        .map(|(contract, name, _)| (contract.to_string(), (*name).to_owned()))
        .collect();

    assert_eq!(
        on_disk, listed,
        "invalid fixtures and EXPECTED_REJECTIONS disagree"
    );
    Ok(())
}

#[test]
fn every_rejection_maps_to_disposition_schema() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let mut rejected = 0_usize;

    for contract in Contract::ALL {
        for (name, bytes) in fixtures(contract, Validity::Invalid)? {
            let disposition = validator
                .read(contract, &bytes)
                .err()
                .map(|error| error.disposition());

            assert_eq!(
                disposition,
                Some(Disposition::Schema),
                "{contract} fixture {name}"
            );
            rejected += 1;
        }
    }

    assert_eq!(
        rejected,
        EXPECTED_REJECTIONS.len(),
        "every invalid fixture was checked"
    );
    Ok(())
}

#[test]
fn rejects_malformed_json() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(Contract::Run, b"{\"schema\": ");

    assert_matches!(
        read,
        Err(ReadContractError::Malformed {
            contract: Contract::Run,
            ..
        })
    );
    Ok(())
}

#[test]
fn rejects_document_without_a_schema_string() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(Contract::Decision, br#"{"schema": 1}"#);

    assert_matches!(
        read,
        Err(ReadContractError::MissingSchema {
            contract: Contract::Decision
        })
    );
    Ok(())
}

#[test]
fn rejects_document_of_another_contract() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let run = fixtures(Contract::Run, Validity::Valid)?;
    let planned = run
        .get("planned")
        .ok_or("run fixture `planned` is missing")?;

    let read = validator.read(Contract::Findings, planned);

    assert_matches!(
        read,
        Err(ReadContractError::UnknownSchema {
            expected: Contract::Findings
        })
    );
    Ok(())
}

#[test]
fn rejects_version_that_is_not_a_number() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(Contract::Event, br#"{"schema": "assay.event/1.1"}"#);

    assert_matches!(
        read,
        Err(ReadContractError::UnknownSchema {
            expected: Contract::Event
        })
    );
    Ok(())
}

#[test]
fn violation_message_does_not_echo_the_rejected_value() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let invalid = fixtures(Contract::Findings, Validity::Invalid)?;
    let bytes = invalid
        .get("over-length-title")
        .ok_or("findings fixture `over-length-title` is missing")?;

    let read = validator.read(Contract::Findings, bytes);

    let Err(ReadContractError::Invalid { violation, .. }) = read else {
        return Err(format!("expected a schema violation, got {read:?}").into());
    };
    assert!(
        !violation.message().contains("xxxxxxxxxx"),
        "untrusted text must not reach error messages: {}",
        violation.message()
    );
    Ok(())
}

/// The closed disposition enum of 03 §5 appears in two schemas; both must list exactly the
/// domain's codes.
#[test]
fn schemas_list_exactly_the_domain_disposition_codes() -> Result<(), Box<dyn Error>> {
    let domain: BTreeSet<String> = Disposition::ALL
        .iter()
        .map(|disposition| disposition.code().to_owned())
        .collect();
    let decision: Value =
        serde_json::from_slice(&fs::read(schemas_dir().join("decision-1.schema.json"))?)?;
    let event: Value =
        serde_json::from_slice(&fs::read(schemas_dir().join("event-1.schema.json"))?)?;

    for (schema, pointer) in [
        (&decision, "/$defs/dispositionCode/enum"),
        (
            &event,
            "/$defs/whenFindingDisposed/then/properties/data/properties/code/enum",
        ),
    ] {
        let listed: BTreeSet<String> = schema
            .pointer(pointer)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("no enum at {pointer}"))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();

        assert_eq!(listed, domain, "disposition codes at {pointer}");
    }
    Ok(())
}

/// The decision over the `rule-8c-llm-degraded` golden inputs with `combined` reported degraded
/// for each of `reasons`, as published JSON.
fn decision_over(reasons: &[CallFailure]) -> Result<Vec<u8>, Box<dyn Error>> {
    decision_with(StageRole::Combined, reasons)
}

/// The decision over the `rule-8c-llm-degraded` golden inputs with `role` reported degraded for
/// each of `reasons`, as published JSON.
fn decision_with(role: StageRole, reasons: &[CallFailure]) -> Result<Vec<u8>, Box<dyn Error>> {
    let inputs = golden_inputs()?;
    let bytes = inputs
        .get("rule-8c-llm-degraded")
        .ok_or("the rule-8c-llm-degraded golden case exists")?;
    let mut inputs: DecisionInputs = serde_json::from_slice(bytes)?;
    inputs.degraded = reasons
        .iter()
        .map(|&reason| DegradedReport { role, reason })
        .collect();
    Ok(serde_json::to_vec(&decide(&rubric_v1()?, &inputs))?)
}

/// #259 F1: every role a report can name reaches `degraded[].role` as a schema-valid code.
#[test]
fn a_decision_over_every_degraded_stage_role_is_schema_valid() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    for role in StageRole::ALL {
        let decision = decision_with(role, &[CallFailure::Cap])?;

        let read = validator.read(Contract::Decision, &decision);

        assert!(read.is_ok(), "{role:?}: {read:?}");
    }
    Ok(())
}

#[test]
fn a_decision_over_every_reported_degradation_reason_is_schema_valid() -> Result<(), Box<dyn Error>>
{
    let validator = ContractValidator::new()?;

    for reason in CallFailure::ALL {
        let decision = decision_over(&[reason])?;
        let read = validator.read(Contract::Decision, &decision);
        assert!(read.is_ok(), "{reason:?}: {read:?}");
    }
    let every = decision_over(&CallFailure::ALL)?;
    let read = validator.read(Contract::Decision, &every);
    assert!(read.is_ok(), "every reason: {read:?}");
    Ok(())
}

/// What #185 found: a decision whose degraded role has an empty reason is not `decision-1`, so
/// the test above would catch an empty reason reaching the decision.
#[test]
fn a_decision_with_an_empty_degraded_reason_is_rejected() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let mut decision: Value = serde_json::from_slice(&decision_over(&[CallFailure::Cap])?)?;
    *decision
        .pointer_mut("/degraded/0/reason")
        .ok_or("the decision lists a degraded role")? = Value::from("");

    let read = validator.read(Contract::Decision, &serde_json::to_vec(&decision)?);

    let error = read.err().ok_or("an empty degraded reason was accepted")?;
    assert_eq!(
        rejection_of(&error),
        Some(Rejection::Violation("/degraded/0/reason", "minLength"))
    );
    Ok(())
}
