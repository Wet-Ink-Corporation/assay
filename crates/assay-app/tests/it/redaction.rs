//! A rejected document is untrusted: neither its values nor the object keys its schema does not
//! name reach the error's `Display` or `Debug` output.

use std::collections::BTreeSet;
use std::error::Error;

use assay_app::{Contract, ContractValidator, ReadContractError};
use serde_json::Value;

use crate::support::{Validity, fixtures, schema};

/// Shorter texts (`full`, `ok`) also occur in the schema's own vocabulary, which messages may
/// quote; from this length on, a match means the document leaked.
const MIN_QUOTE_LEN: usize = 4;

/// A producer-chosen key: long, secret-like, and named by no schema. The fixture
/// `run/invalid/empty-artifact-name.json` uses it as an `artifact_names` key.
const SECRET_KEY: &str =
    "ghp_R7d2kQ9xLm4ZpT8vWc1NbY6sHf3JgE0aUoXi_secret_like_key_material_0123456789";

fn assert_never_quoted(error: &ReadContractError, secret: &str) {
    let display = error.to_string();
    let debug = format!("{error:?}");

    assert!(!display.contains(secret), "Display quotes it: {display}");
    assert!(!debug.contains(secret), "Debug quotes it: {debug}");
}

#[test]
fn unknown_field_error_counts_the_field_without_naming_it() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let run = fixtures(Contract::Run, Validity::Valid)?;
    let mut planned: Value = serde_json::from_slice(
        run.get("planned")
            .ok_or("run fixture `planned` is missing")?,
    )?;
    planned
        .as_object_mut()
        .ok_or("`planned` is not an object")?
        .insert(SECRET_KEY.to_owned(), Value::Bool(true));

    let read = validator.read(Contract::Run, &serde_json::to_vec(&planned)?);

    let error = read.err().ok_or("an unknown field must be rejected")?;
    assert_never_quoted(&error, SECRET_KEY);
    let ReadContractError::Invalid { violation, .. } = error else {
        return Err(format!("expected a schema violation, got {error:?}").into());
    };
    assert_eq!(
        (
            violation.instance_path(),
            violation.keyword(),
            violation.message()
        ),
        (
            "",
            "additionalProperties",
            "1 unexpected property is not allowed"
        )
    );
    Ok(())
}

#[test]
fn repeated_key_error_never_quotes_the_key() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let document = format!(r#"{{"schema": "assay.run/1", "{SECRET_KEY}": 1, "{SECRET_KEY}": 2}}"#);

    let read = validator.read(Contract::Run, document.as_bytes());

    let error = read.err().ok_or("a repeated key must be rejected")?;
    assert_never_quoted(&error, SECRET_KEY);
    Ok(())
}

/// Every text of `document` that only the producer chose: each string value, and each object
/// key the schema does not declare, of at least [`MIN_QUOTE_LEN`] characters. The `schema`
/// field is left out: its contract and version are reported, parsed, by design.
fn producer_texts<'a>(
    document: &'a Value,
    declared: &BTreeSet<&str>,
    texts: &mut BTreeSet<&'a str>,
) {
    match document {
        Value::Object(map) => {
            for (key, value) in map {
                if !declared.contains(key.as_str()) && key.chars().count() >= MIN_QUOTE_LEN {
                    texts.insert(key);
                }
                if key != "schema" {
                    producer_texts(value, declared, texts);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                producer_texts(item, declared, texts);
            }
        }
        Value::String(text) if text.chars().count() >= MIN_QUOTE_LEN => {
            texts.insert(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Every key of every `properties` map in `schema`: the names a message may quote.
fn declared_names<'a>(schema: &'a Value, names: &mut BTreeSet<&'a str>) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                names.extend(properties.keys().map(String::as_str));
            }
            for value in map.values() {
                declared_names(value, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                declared_names(item, names);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[test]
fn no_rejection_of_an_invalid_fixture_quotes_producer_text() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let mut checked = 0_usize;

    for contract in Contract::ALL {
        let schema = schema(schema_stem(contract))?;
        let mut declared = BTreeSet::new();
        declared_names(&schema, &mut declared);
        for (name, bytes) in fixtures(contract, Validity::Invalid)? {
            let document: Value = serde_json::from_slice(&bytes)?;
            let mut texts = BTreeSet::new();
            producer_texts(&document, &declared, &mut texts);

            let error = validator
                .read(contract, &bytes)
                .err()
                .ok_or_else(|| format!("{contract} fixture {name} must be rejected"))?;

            let display = error.to_string();
            let debug = format!("{error:?}");
            for text in &texts {
                assert!(
                    !display.contains(text) && !debug.contains(text),
                    "{contract} fixture {name} quotes {text:?}: {display} / {debug}"
                );
            }
            checked += 1;
        }
    }

    assert!(checked > 0, "no invalid fixture was checked");
    Ok(())
}

fn schema_stem(contract: Contract) -> &'static str {
    match contract {
        Contract::Findings => "findings-1",
        Contract::Run => "run-1",
        Contract::Decision => "decision-1",
        Contract::Event => "event-1",
    }
}

#[test]
fn error_under_an_open_map_never_quotes_its_key() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let invalid = fixtures(Contract::Run, Validity::Invalid)?;
    let bytes = invalid
        .get("empty-artifact-name")
        .ok_or("run fixture `empty-artifact-name` is missing")?;

    let read = validator.read(Contract::Run, bytes);

    let error = read
        .err()
        .ok_or("an empty artifact name must be rejected")?;
    assert_never_quoted(&error, SECRET_KEY);
    Ok(())
}
