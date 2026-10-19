//! The contract types read every valid golden fixture of `schemas/fixtures/` and write it back
//! unchanged, and refuse, each with its own reason, the invalid fixtures whose fault is one of
//! shape (unknown field or version, missing or mistyped field) or the one conditional rule the
//! domain shares with the schema (a `FindingDisposed` without the fingerprint its code
//! requires, #170). Other conditional requirements and length caps are the schema's alone,
//! checked by `assay-app` on read. The `domain-invalid` fixtures are the reverse: the schema
//! accepts them and the domain refuses them (data on a kind whose fields 03 §6 does not list,
//! #147).

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Debug;

use assay_domain::{Decision, Event, FindingsDocument};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::support::{domain_invalid_fixtures, invalid_fixtures, valid_fixtures};

/// Each invalid event fixture, and a fragment of the message the domain refuses it with.
const EVENT_REJECTIONS: [(&str, &str); 15] = [
    ("command-voided-unknown-reason", "unknown variant `expired`"),
    ("data-not-an-object", "invalid type"),
    (
        "finding-disposed-ceiling-without-fp",
        "a `cap-exceeded` disposition must name its finding's fingerprint",
    ),
    (
        "finding-disposed-downgraded-without-fp",
        "a `verifier-rejected-downgraded` disposition must name its finding's fingerprint",
    ),
    (
        "finding-disposed-duplicate-without-fp",
        "a `duplicate` disposition must name its finding's fingerprint",
    ),
    (
        "finding-disposed-moved-without-fp",
        "a `line-not-in-diff-moved` disposition must name its finding's fingerprint",
    ),
    ("missing-run-uid", "missing field `run_uid`"),
    (
        "preflight-builtins-unknown-field",
        "unknown field `mcp_servers`",
    ),
    ("push-recorded-unknown-data-field", "unknown field `at`"),
    ("run-uid-unknown-field", "unknown field `run_number`"),
    ("slot-reaped-without-run-uid", "missing field `run_uid`"),
    ("unknown-data-field", "unknown field `signal`"),
    ("unknown-field", "unknown field `seq`"),
    ("unknown-version", "unknown variant `assay.event/2`"),
    (
        "waiver-applied-uppercase-fp",
        "a fingerprint is lowercase hex, but byte 0 is not",
    ),
];

/// Each event fixture the schema accepts and the domain refuses, and the message fragment.
const EVENT_DOMAIN_REJECTIONS: [(&str, &str); 1] =
    [("run-requested-with-data", "unknown field `trigger`")];

/// Each invalid decision fixture, and the message fragment.
const DECISION_REJECTIONS: [(&str, &str); 2] = [
    ("unknown-field", "unknown field `praise`"),
    ("unknown-version", "unknown variant `assay.decision/7`"),
];

/// The invalid findings fixtures whose fault is one of shape, and the message fragment; the
/// others break caps and conditional rules only the schema states.
const FINDINGS_REJECTIONS: [(&str, &str); 4] = [
    ("unknown-field", "unknown field `severity_hint`"),
    ("unknown-version", "unknown variant `assay.findings/2`"),
    ("producer-fp", "unknown field `fp`"),
    ("producer-group-id", "unknown field `group_id`"),
];

/// Asserts that `T` refuses each fixture of `expected` from `fixtures` as a data error whose
/// message holds its fragment.
fn assert_refused<T: DeserializeOwned + Debug>(
    fixtures: &BTreeMap<String, Vec<u8>>,
    expected: &[(&str, &str)],
) -> Result<(), Box<dyn Error>> {
    for &(name, fragment) in expected {
        let bytes = fixtures
            .get(name)
            .ok_or_else(|| format!("no fixture {name}"))?;

        let read = serde_json::from_slice::<T>(bytes);

        assert!(
            matches!(&read, Err(e) if e.is_data() && e.to_string().contains(fragment)),
            "{name}: expected a data error holding {fragment:?}, got {read:?}"
        );
    }
    Ok(())
}

/// The fixture names of `fixtures`, and those `expected` lists.
fn names(
    fixtures: &BTreeMap<String, Vec<u8>>,
    expected: &[(&str, &str)],
) -> (BTreeSet<String>, BTreeSet<String>) {
    (
        fixtures.keys().cloned().collect(),
        expected.iter().map(|&(name, _)| name.to_owned()).collect(),
    )
}

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
    T: Serialize + DeserializeOwned + PartialEq + Debug,
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

/// Every invalid decision fixture is a shape fault, so the domain type refuses all of them,
/// each for its own reason.
#[test]
fn every_invalid_decision_fixture_is_refused_for_its_reason() -> Result<(), Box<dyn Error>> {
    let fixtures = invalid_fixtures("decision")?;

    let (on_disk, listed) = names(&fixtures, &DECISION_REJECTIONS);

    assert_eq!(
        on_disk, listed,
        "invalid decision fixtures and DECISION_REJECTIONS"
    );
    assert_refused::<Decision>(&fixtures, &DECISION_REJECTIONS)
}

#[test]
fn findings_with_a_foreign_field_or_version_are_refused_for_their_reason()
-> Result<(), Box<dyn Error>> {
    let fixtures = invalid_fixtures("findings")?;

    assert_refused::<FindingsDocument>(&fixtures, &FINDINGS_REJECTIONS)
}

/// Every invalid event fixture breaks a shape or the fingerprint rule, so the domain type
/// refuses all of them, each for its own reason.
#[test]
fn every_invalid_event_fixture_is_refused_for_its_reason() -> Result<(), Box<dyn Error>> {
    let fixtures = invalid_fixtures("event")?;

    let (on_disk, listed) = names(&fixtures, &EVENT_REJECTIONS);

    assert_eq!(
        on_disk, listed,
        "invalid event fixtures and EVENT_REJECTIONS"
    );
    assert_refused::<Event>(&fixtures, &EVENT_REJECTIONS)
}

/// #147: the schema leaves `data` open for the kinds 03 §6 lists no fields for, and the domain
/// refuses any field there, so a field added to one needs every reader upgraded first.
#[test]
fn data_on_a_field_less_kind_is_refused_by_the_domain() -> Result<(), Box<dyn Error>> {
    let fixtures = domain_invalid_fixtures("event")?;

    let (on_disk, listed) = names(&fixtures, &EVENT_DOMAIN_REJECTIONS);

    assert_eq!(on_disk, listed, "domain-invalid event fixtures");
    assert_refused::<Event>(&fixtures, &EVENT_DOMAIN_REJECTIONS)
}
