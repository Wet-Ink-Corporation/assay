//! Validate-on-read for the versioned contracts (`docs/architecture/03-contracts.md` preamble,
//! §3 to §6) against the golden fixtures in `schemas/fixtures/`.

use std::assert_matches;
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;

use assay_app::{Contract, ContractValidator, ReadContractError};
use assay_domain::Disposition;
use serde_json::Value;

use crate::support::{Validity, fixtures, schemas_dir};

/// How each invalid fixture must be rejected: `Version(n)` for an unknown version, otherwise the
/// JSON Pointer of the offending instance and the schema keyword that rejects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejection<'a> {
    Version(u32),
    Violation(&'a str, &'a str),
}

const EXPECTED_REJECTIONS: [(Contract, &str, Rejection<'static>); 38] = [
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
        "command-voided-unknown-reason",
        Rejection::Violation("/data/reason", "enum"),
    ),
    (
        Contract::Event,
        "slot-reaped-without-run-uid",
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
