//! decide-only's decision bundle read back (`assay.run-decision/1`, #61 DO-6): the reader
//! accepts exactly what `prepare_decision` writes, and agrees with the schema on every golden.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    Contract, ContractValidator, DecisionBundleError, MAX_DECISION_BUNDLE_BYTES,
    read_decision_bundle,
};
use serde_json::{Value, json};

use crate::eligibility_fakes::OWNER;
use crate::support::{Validity, fixtures};

#[test]
fn the_reader_and_the_schema_agree_on_every_golden() -> Result<(), Box<dyn Error>> {
    let valid = fixtures(Contract::RunDecision, Validity::Valid)?;
    let invalid = fixtures(Contract::RunDecision, Validity::Invalid)?;
    assert!(!valid.is_empty() && !invalid.is_empty(), "goldens on disk");

    for (name, bytes) in valid {
        let read = read_decision_bundle(&bytes);
        assert!(read.is_ok(), "valid {name}: {read:?}");
    }
    for (name, bytes) in invalid {
        let read = read_decision_bundle(&bytes);
        assert_matches!(
            read,
            Err(DecisionBundleError::Unreadable(_)),
            "invalid {name}"
        );
    }
    Ok(())
}

/// One edit of a written bundle the reader must refuse, though its schema allows it.
type Tampering = (&'static str, fn(&mut Value));

#[test]
fn the_reader_refuses_what_the_writer_never_writes() -> Result<(), Box<dyn Error>> {
    let golden = fixtures(Contract::RunDecision, Validity::Valid)?;
    let comment_golden = golden.get("comment").ok_or("the comment golden")?;
    let written: Value = serde_json::from_slice(comment_golden)?;
    let cases: [Tampering; 9] = [
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
    let validator = ContractValidator::new()?;
    for (name, tamper) in cases {
        // Each case edits its own copy of the golden.
        let mut bundle = written.clone();
        tamper(&mut bundle);
        let bytes = serde_json::to_vec(&bundle)?;
        assert!(
            validator.read(Contract::RunDecision, &bytes).is_ok(),
            "{name}: the schema allows it"
        );

        let read = read_decision_bundle(&bytes);

        assert_matches!(read, Err(DecisionBundleError::Unreadable(_)), "{name}");
    }
    Ok(())
}

#[test]
fn a_bundle_past_its_cap_is_refused_unparsed() {
    let bytes = vec![b' '; MAX_DECISION_BUNDLE_BYTES + 1];

    let read = read_decision_bundle(&bytes);

    assert_matches!(read, Err(DecisionBundleError::TooLarge));
}
