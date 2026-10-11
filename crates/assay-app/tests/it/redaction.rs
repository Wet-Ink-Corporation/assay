//! A rejected document is untrusted: neither its values nor the object keys its schema does not
//! name reach the error's `Display` or `Debug` output.

use std::error::Error;

use assay_app::{Contract, ContractValidator, ReadContractError, SchemaViolation};
use serde_json::Value;

use crate::support::{Validity, fixtures};

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
        violation,
        SchemaViolation {
            instance_path: String::new(),
            keyword: "additionalProperties".to_owned(),
            message: "1 unexpected property is not allowed".to_owned(),
        }
    );
    Ok(())
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
