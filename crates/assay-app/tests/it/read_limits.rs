//! What `ContractValidator::read` refuses before the schema runs: a document over the size cap
//! (H-13), nesting past `serde_json`'s recursion limit, and an object that repeats a key, which
//! parsers resolve differently (OUT-05).

use std::assert_matches;
use std::error::Error;

use assay_app::{Contract, ContractValidator, MAX_DOCUMENT_BYTES, ReadContractError};
use serde_json::Value;

use crate::support::{Validity, fixtures};

fn planned_run() -> Result<Vec<u8>, Box<dyn Error>> {
    fixtures(Contract::Run, Validity::Valid)?
        .remove("planned")
        .ok_or_else(|| "run fixture `planned` is missing".into())
}

#[test]
fn rejects_a_document_one_byte_over_the_cap_as_too_large() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let over = MAX_DOCUMENT_BYTES + 1;
    let document = vec![b' '; over];

    let read = validator.read(Contract::Run, &document);

    assert_matches!(
        read,
        Err(ReadContractError::TooLarge {
            contract: Contract::Run,
            size,
        }) if size == over
    );
    Ok(())
}

#[test]
fn accepts_a_valid_document_of_exactly_the_cap() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let mut document = planned_run()?;
    let expected: Value = serde_json::from_slice(&document)?;
    document.resize(MAX_DOCUMENT_BYTES, b' ');

    let read = validator.read(Contract::Run, &document);

    assert_eq!(read.ok(), Some(expected));
    Ok(())
}

#[test]
fn rejects_a_repeated_top_level_key_as_malformed() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(
        Contract::Run,
        br#"{"schema": "assay.run/1", "schema": "assay.run/1"}"#,
    );

    assert_matches!(
        read,
        Err(ReadContractError::Malformed {
            contract: Contract::Run,
            ..
        })
    );
    Ok(())
}

/// The second `gates-lint` would otherwise silently replace the first, and a parser that keeps
/// the first value would act on a different document than the one validated.
#[test]
fn rejects_a_repeated_key_in_a_nested_object_as_malformed() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let text = String::from_utf8(planned_run()?)?;
    let repeated = text.replacen(
        r#""gates-lint": "gates-lint-3f9a""#,
        r#""gates-lint": "gates-lint-3f9a", "gates-lint": "gates-lint-0000""#,
        1,
    );
    assert_ne!(repeated, text, "the fixture names a `gates-lint` artifact");

    let read = validator.read(Contract::Run, repeated.as_bytes());

    assert_matches!(
        read,
        Err(ReadContractError::Malformed {
            contract: Contract::Run,
            ..
        })
    );
    Ok(())
}

/// The deepest nesting of arrays or objects `serde_json` parses. Its recursion limit is 128,
/// counted down on entry to each array or object, and the one that reaches zero is refused, so
/// 127 levels parse and 128 do not. `read` relies on that limit to bound the strict visitor's
/// recursion (`src/contract/strict.rs`), which adds none of its own; a `serde_json` that dropped
/// it (its `unbounded_depth` feature) or moved it would turn these red (TST-18).
const DEEPEST_ACCEPTED_NESTING: usize = 127;

fn nested_arrays(depth: usize) -> Vec<u8> {
    [b"[".repeat(depth), b"]".repeat(depth)].concat()
}

fn nested_objects(depth: usize) -> Vec<u8> {
    [
        br#"{"a":"#.repeat(depth),
        b"null".to_vec(),
        b"}".repeat(depth),
    ]
    .concat()
}

#[test]
fn parses_arrays_nested_to_serde_json_s_limit() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(Contract::Run, &nested_arrays(DEEPEST_ACCEPTED_NESTING));

    assert_matches!(
        read,
        Err(ReadContractError::MissingSchema {
            contract: Contract::Run
        })
    );
    Ok(())
}

#[test]
fn rejects_arrays_nested_one_past_serde_json_s_limit_as_malformed() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(Contract::Run, &nested_arrays(DEEPEST_ACCEPTED_NESTING + 1));

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
fn parses_objects_nested_to_serde_json_s_limit() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(Contract::Run, &nested_objects(DEEPEST_ACCEPTED_NESTING));

    assert_matches!(
        read,
        Err(ReadContractError::MissingSchema {
            contract: Contract::Run
        })
    );
    Ok(())
}

#[test]
fn rejects_objects_nested_one_past_serde_json_s_limit_as_malformed() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    let read = validator.read(Contract::Run, &nested_objects(DEEPEST_ACCEPTED_NESTING + 1));

    assert_matches!(
        read,
        Err(ReadContractError::Malformed {
            contract: Contract::Run,
            ..
        })
    );
    Ok(())
}
