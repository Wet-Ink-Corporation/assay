//! The contract types read every valid golden fixture of `schemas/fixtures/` and write it back
//! unchanged, and refuse the invalid fixtures whose fault is one of shape (unknown field or
//! version, missing or mistyped field). Length caps and conditional requirements belong to the
//! schema, which `assay-app` checks on read.

use std::error::Error;

use assay_domain::{Decision, Event, FindingsDocument};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::support::{invalid_fixtures, valid_fixtures};

/// Drops every `"pre_existing": false`: absent and `false` mean the same, and the domain always
/// writes the field.
fn without_default_flags(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("pre_existing") == Some(&Value::Bool(false)) {
                map.remove("pre_existing");
            }
            map.values_mut().for_each(without_default_flags);
        }
        Value::Array(items) => items.iter_mut().for_each(without_default_flags),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Reads `bytes` as `T`, writes it back, and asserts the JSON is the fixture's and reads back
/// to the same value.
fn assert_round_trips<T>(name: &str, bytes: &[u8]) -> Result<(), Box<dyn Error>>
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let mut original: Value = serde_json::from_slice(bytes)?;
    let read: T = serde_json::from_slice(bytes).map_err(|e| format!("{name}: {e}"))?;

    let mut written = serde_json::to_value(&read)?;
    let reread = T::deserialize(&written)?;

    assert_eq!(reread, read, "{name}: write then read");
    without_default_flags(&mut original);
    without_default_flags(&mut written);
    assert_eq!(written, original, "{name}: written JSON");
    Ok(())
}

#[test]
fn every_valid_findings_fixture_round_trips() -> Result<(), Box<dyn Error>> {
    let fixtures = valid_fixtures("findings")?;
    assert!(!fixtures.is_empty(), "no valid findings fixtures");

    for (name, bytes) in &fixtures {
        assert_round_trips::<FindingsDocument>(name, bytes)?;
    }
    Ok(())
}

#[test]
fn every_valid_event_fixture_round_trips() -> Result<(), Box<dyn Error>> {
    let fixtures = valid_fixtures("event")?;
    assert!(!fixtures.is_empty(), "no valid event fixtures");

    for (name, bytes) in &fixtures {
        assert_round_trips::<Event>(name, bytes)?;
    }
    Ok(())
}

#[test]
fn every_valid_decision_fixture_round_trips() -> Result<(), Box<dyn Error>> {
    let fixtures = valid_fixtures("decision")?;
    assert!(!fixtures.is_empty(), "no valid decision fixtures");

    for (name, bytes) in &fixtures {
        assert_round_trips::<Decision>(name, bytes)?;
    }
    Ok(())
}

/// Every invalid decision fixture is a shape fault, so the domain type refuses all of them.
#[test]
fn every_invalid_decision_fixture_is_refused() -> Result<(), Box<dyn Error>> {
    let fixtures = invalid_fixtures("decision")?;
    assert!(!fixtures.is_empty(), "no invalid decision fixtures");

    for (name, bytes) in &fixtures {
        let read = serde_json::from_slice::<Decision>(bytes);

        assert!(
            matches!(&read, Err(e) if e.is_data()),
            "{name}: expected a data error, got {read:?}"
        );
    }
    Ok(())
}

#[test]
fn findings_with_a_foreign_field_or_version_are_refused() -> Result<(), Box<dyn Error>> {
    let fixtures = invalid_fixtures("findings")?;

    for name in [
        "unknown-field",
        "unknown-version",
        "producer-fp",
        "producer-group-id",
    ] {
        let bytes = fixtures
            .get(name)
            .ok_or_else(|| format!("no fixture {name}"))?;

        let read = serde_json::from_slice::<FindingsDocument>(bytes);

        assert!(
            matches!(&read, Err(e) if e.is_data()),
            "{name}: expected a data error, got {read:?}"
        );
    }
    Ok(())
}

/// Every invalid event fixture is a shape fault, so the domain type refuses all of them.
#[test]
fn every_invalid_event_fixture_is_refused() -> Result<(), Box<dyn Error>> {
    let fixtures = invalid_fixtures("event")?;
    assert!(!fixtures.is_empty(), "no invalid event fixtures");

    for (name, bytes) in &fixtures {
        let read = serde_json::from_slice::<Event>(bytes);

        assert!(
            matches!(&read, Err(e) if e.is_data()),
            "{name}: expected a data error, got {read:?}"
        );
    }
    Ok(())
}
