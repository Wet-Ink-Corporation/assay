//! A rejected document is untrusted: neither its values nor the object keys its schema does not
//! name reach the error's `Display` or `Debug` output.

use std::collections::BTreeSet;
use std::error::Error;

use assay_app::{Contract, ContractValidator, ReadContractError};
use serde_json::{Value, json};

use crate::support::{Validity, fixtures, schema};

/// Shorter texts (`ci`, `fmt`, `v1`) also occur in the schema's own vocabulary and in the
/// validator's wording, which messages may quote; from this length on, a match means the document
/// leaked.
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
/// key the schema does not declare, of at least [`MIN_QUOTE_LEN`] characters. The root's `schema`
/// field is left out: its contract and version are reported, parsed, by design. A `schema` key
/// anywhere else is the producer's like any other.
fn producer_texts<'a>(document: &'a Value, declared: &BTreeSet<&str>) -> BTreeSet<&'a str> {
    let mut texts = BTreeSet::new();
    match document {
        Value::Object(root) => {
            for (key, value) in root.iter().filter(|(key, _)| key.as_str() != "schema") {
                member_texts(key, value, declared, &mut texts);
            }
        }
        other => value_texts(other, declared, &mut texts),
    }
    texts
}

fn member_texts<'a>(
    key: &'a str,
    value: &'a Value,
    declared: &BTreeSet<&str>,
    texts: &mut BTreeSet<&'a str>,
) {
    if !declared.contains(key) && key.chars().count() >= MIN_QUOTE_LEN {
        texts.insert(key);
    }
    value_texts(value, declared, texts);
}

fn value_texts<'a>(value: &'a Value, declared: &BTreeSet<&str>, texts: &mut BTreeSet<&'a str>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                member_texts(key, value, declared, texts);
            }
        }
        Value::Array(items) => {
            for item in items {
                value_texts(item, declared, texts);
            }
        }
        Value::String(text) if text.chars().count() >= MIN_QUOTE_LEN => {
            texts.insert(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Asserts that `error`, the rejection of `document`, quotes none of its producer texts.
fn assert_quotes_no_producer_text(
    label: &str,
    document: &Value,
    declared: &BTreeSet<&str>,
    error: &ReadContractError,
) {
    let display = error.to_string();
    let debug = format!("{error:?}");
    for text in producer_texts(document, declared) {
        assert!(
            !display.contains(text) && !debug.contains(text),
            "{label} quotes {text:?}: {display} / {debug}"
        );
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

            let error = validator
                .read(contract, &bytes)
                .err()
                .ok_or_else(|| format!("{contract} fixture {name} must be rejected"))?;

            let label = format!("{contract} fixture {name}");
            assert_quotes_no_producer_text(&label, &document, &declared, &error);
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
        Contract::Status => "status-1",
        Contract::RunDecision => "run-decision-1",
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

/// A valid fixture broken at one schema keyword by a distinctive producer text (or number) the
/// rejection must not quote. The invalid fixtures reach only some of these keywords, and not
/// always through a text of [`MIN_QUOTE_LEN`] or more.
struct Breach {
    contract: Contract,
    fixture: &'static str,
    /// Members to set: the JSON Pointer of an object member, and its new value.
    edits: Vec<(&'static str, Value)>,
    keyword: &'static str,
    secret: &'static str,
}

fn breaches() -> [Breach; 5] {
    [
        Breach {
            contract: Contract::Event,
            fixture: "run-requested",
            edits: vec![("/ts", json!("ts-QZXV-not-a-date-time"))],
            keyword: "format",
            secret: "ts-QZXV-not-a-date-time",
        },
        Breach {
            contract: Contract::Run,
            fixture: "planned",
            edits: vec![("/head_sha", json!("sha-QZXV-not-forty-hex"))],
            keyword: "pattern",
            secret: "sha-QZXV-not-forty-hex",
        },
        Breach {
            contract: Contract::Run,
            fixture: "planned",
            edits: vec![
                ("/plugins/0/pin", json!("pin-QZXV-and-a-job-too")),
                ("/plugins/0/job", json!("gates-lint")),
            ],
            keyword: "oneOf",
            secret: "pin-QZXV-and-a-job-too",
        },
        Breach {
            contract: Contract::Run,
            fixture: "planned",
            edits: vec![("/classes", json!(["cls-QZXV-twice", "cls-QZXV-twice"]))],
            keyword: "uniqueItems",
            secret: "cls-QZXV-twice",
        },
        Breach {
            contract: Contract::Run,
            fixture: "planned",
            edits: vec![("/pr", json!(-77_317_731))],
            keyword: "minimum",
            secret: "77317731",
        },
    ]
}

/// Sets the object member `pointer` names in `document` to `value`.
fn set_member(document: &mut Value, pointer: &str, value: Value) -> Result<(), Box<dyn Error>> {
    let (parent, key) = pointer
        .rsplit_once('/')
        .ok_or_else(|| format!("{pointer} is not a JSON Pointer"))?;
    document
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("{parent} is not an object"))?
        .insert(key.to_owned(), value);
    Ok(())
}

#[test]
fn rejections_by_format_pattern_one_of_unique_items_and_minimum_quote_no_producer_text()
-> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;

    for breach in breaches() {
        let schema = schema(schema_stem(breach.contract))?;
        let mut declared = BTreeSet::new();
        declared_names(&schema, &mut declared);
        let mut document: Value = serde_json::from_slice(
            fixtures(breach.contract, Validity::Valid)?
                .get(breach.fixture)
                .ok_or_else(|| {
                    format!("{} fixture {} is missing", breach.contract, breach.fixture)
                })?,
        )?;
        for (pointer, value) in breach.edits {
            set_member(&mut document, pointer, value)?;
        }

        let read = validator.read(breach.contract, &serde_json::to_vec(&document)?);

        let error = read
            .err()
            .ok_or_else(|| format!("the {} breach must be rejected", breach.keyword))?;
        let ReadContractError::Invalid { violation, .. } = &error else {
            return Err(format!("expected a {} violation, got {error:?}", breach.keyword).into());
        };
        assert_eq!(violation.keyword(), breach.keyword);
        assert_never_quoted(&error, breach.secret);
        let label = format!("the {} breach", breach.keyword);
        assert_quotes_no_producer_text(&label, &document, &declared, &error);
    }
    Ok(())
}
