//! decide-only's decision bundle read back (`assay.run-decision/1`, #61 DO-6): the reader
//! accepts exactly what `prepare_decision` writes, and agrees with the schema on every rule.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    BundleShape, Contract, ContractValidator, DecisionBundleError, MAX_DECISION_BUNDLE_BYTES,
    read_decision_bundle,
};
use serde_json::{Value, json};

use crate::eligibility_fakes::OWNER;
use crate::support::{Validity, fixtures};

/// One edit of a written bundle, breaking exactly one rule.
type Tampering = (&'static str, fn(&mut Value));

/// The valid golden `name`, parsed.
fn golden(name: &str) -> Result<Value, Box<dyn Error>> {
    let valid = fixtures(Contract::RunDecision, Validity::Valid)?;
    let bytes = valid
        .get(name)
        .ok_or_else(|| format!("the {name} golden"))?;
    Ok(serde_json::from_slice(bytes)?)
}

/// `count` identities with ids from `first` and logins `<prefix><n>`.
fn identities(first: u64, prefix: &str, count: u64) -> Value {
    (0..count)
        .map(|n| json!({"id": first + n, "login": format!("{prefix}{n}")}))
        .collect()
}

/// Asserts that every edit of `written` is refused by the reader as not the contract's shape,
/// and by the schema exactly when `schema_refuses`.
fn assert_each_refused(
    written: &Value,
    cases: &[Tampering],
    schema_refuses: bool,
) -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let unedited = serde_json::to_vec(written)?;
    assert!(
        read_decision_bundle(&unedited).is_ok()
            && validator.read(Contract::RunDecision, &unedited).is_ok(),
        "the unedited golden reads, so each case fails for its own edit"
    );
    for (name, tamper) in cases {
        // Each case edits its own copy of the golden.
        let mut bundle = written.clone();
        tamper(&mut bundle);
        let bytes = serde_json::to_vec(&bundle)?;
        assert_eq!(
            validator.read(Contract::RunDecision, &bytes).is_err(),
            schema_refuses,
            "{name}: the schema's verdict"
        );

        let read = read_decision_bundle(&bytes);

        assert_matches!(
            read,
            Err(DecisionBundleError::Unreadable(BundleShape::Shape)),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn every_valid_golden_reads() -> Result<(), Box<dyn Error>> {
    let valid = fixtures(Contract::RunDecision, Validity::Valid)?;
    assert!(!valid.is_empty(), "goldens on disk");

    for (name, bytes) in valid {
        let read = read_decision_bundle(&bytes);

        assert!(read.is_ok(), "valid {name}: {read:?}");
    }
    Ok(())
}

#[test]
fn the_reader_refuses_each_rule_the_schema_states() -> Result<(), Box<dyn Error>> {
    let cases: [Tampering; 10] = [
        ("evidence of attempt 2", |bundle| {
            bundle["evidence"]["run_attempt"] = json!(2);
        }),
        ("an uppercase pointer digest", |bundle| {
            let upper = bundle["evidence"]["sha256"]
                .as_str()
                .map(|digest| digest.replacen('b', "B", 1));
            bundle["evidence"]["sha256"] = json!(upper);
        }),
        ("a run of attempt 2", |bundle| {
            bundle["run_uid"]["run_attempt"] = json!(2);
            // The events name the same run, so only the run's attempt rule refuses it.
            if let Some(events) = bundle["events"].as_array_mut() {
                for event in events {
                    event["run_uid"]["run_attempt"] = json!(2);
                }
            }
        }),
        ("an unknown top-level key", |bundle| {
            bundle["extra"] = json!(true);
        }),
        ("an unknown key in the trigger", |bundle| {
            bundle["trigger"]["extra"] = json!(true);
        }),
        ("another version", |bundle| {
            bundle["schema"] = json!("assay.run-decision/2");
        }),
        ("65 owners", |bundle| {
            bundle["github"]["lists"]["owners"] = identities(6000, "o", 65);
        }),
        ("129 maintainers", |bundle| {
            bundle["github"]["lists"]["maintainers"] = identities(7000, "m", 129);
        }),
        ("an event of another job", |bundle| {
            bundle["events"][0]["job"] = json!("decide");
        }),
        ("an event of another kind", |bundle| {
            bundle["events"][0]["kind"] = json!("DecisionMade");
            bundle["events"][0]["data"] = json!({});
        }),
    ];

    assert_each_refused(&golden("comment")?, &cases, true)
}

#[test]
fn the_reader_refuses_a_scheduled_trigger_with_an_actor() -> Result<(), Box<dyn Error>> {
    let cases: [Tampering; 1] = [("schedule with actor_id", |bundle| {
        bundle["trigger"]["actor_id"] = json!(OWNER);
    })];

    assert_each_refused(&golden("schedule")?, &cases, true)
}

#[test]
fn the_reader_refuses_what_the_writer_never_writes() -> Result<(), Box<dyn Error>> {
    let cases: [Tampering; 10] = [
        ("out of order", |bundle| {
            if let Some(events) = bundle["events"].as_array_mut() {
                events.swap(2, 3);
                events[2]["idx"] = json!(2);
                events[3]["idx"] = json!(3);
            }
        }),
        ("a gap in idx", |bundle| {
            bundle["events"][1]["idx"] = json!(5);
        }),
        ("another stage", |bundle| {
            bundle["events"][0]["stage"] = json!("S8");
        }),
        ("another run", |bundle| {
            bundle["events"][0]["run_uid"]["run_id"] = json!(100);
        }),
        ("another run key", |bundle| {
            bundle["events"][0]["run_key"] = json!("sha256:other");
        }),
        ("a time unlike the first event's", |bundle| {
            bundle["events"][2]["ts"] = json!("2026-11-01T10:00:01Z");
        }),
        ("a command of another comment", |bundle| {
            bundle["events"][0]["data"]["comment_id"] = json!(73);
        }),
        ("a command event without a comment trigger", |bundle| {
            bundle["trigger"] = json!({"event": "label", "actor_id": OWNER});
        }),
        ("a decision key that is no digest", |bundle| {
            bundle["inputs"]["decision_key"] = json!("dec-1");
        }),
        (
            "a decision of another rubric version than recorded",
            |bundle| {
                bundle["rubric_version"] = json!("2");
            },
        ),
    ];

    assert_each_refused(&golden("comment")?, &cases, false)
}

#[test]
fn the_reader_refuses_events_that_do_not_say_what_the_bundle_says() -> Result<(), Box<dyn Error>> {
    let cases: [Tampering; 4] = [
        ("a voided record that is not void", |bundle| {
            bundle["events"][1]["data"]["comment_id"] = json!(71);
        }),
        ("a void record left without its event", |bundle| {
            if let Some(events) = bundle["events"].as_array_mut() {
                events.remove(1);
                events[1]["idx"] = json!(1);
                events[2]["idx"] = json!(2);
            }
        }),
        ("a waiver the decision did not apply", |bundle| {
            bundle["events"][2]["data"]["who"] = json!("break-glass");
        }),
        (
            "an override the decision applied, without its event",
            |bundle| {
                if let Some(events) = bundle["events"].as_array_mut() {
                    events.pop();
                }
            },
        ),
    ];

    assert_each_refused(&golden("comment")?, &cases, false)
}

#[test]
fn a_bundle_past_its_cap_is_refused_unparsed() {
    let bytes = vec![b' '; MAX_DECISION_BUNDLE_BYTES + 1];

    let read = read_decision_bundle(&bytes);

    assert_matches!(read, Err(DecisionBundleError::TooLarge));
}
