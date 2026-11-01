use std::assert_matches;

use proptest::prelude::*;

use super::*;
use crate::ci::Outputs;

const LEASE: &str = "4444444444444444444444444444444444444444";

fn slot(index: u8) -> Slot {
    Slot::new(index).expect("slots 0 and 1 exist")
}

fn held(index: u8) -> HeldSlot {
    HeldSlot {
        slot: slot(index),
        lease_commit: LEASE.parse().expect("a full commit id"),
    }
}

/// `toJSON(needs)` for an `admit` that ended with `result` and wrote `outputs`, and a review
/// beside it.
fn needs_text(result: &str, outputs: &[(&str, &str)]) -> String {
    let outputs: serde_json::Map<String, serde_json::Value> = outputs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::json!({ "admit": { "result": result, "outputs": outputs } }).to_string()
}

fn decoded_after(result: &str, outputs: &[(&str, &str)]) -> Result<Option<HeldSlot>, NeedsError> {
    let needs = Needs::parse(needs_text(result, outputs).as_bytes(), &[JobName::Admit])?;
    HeldSlot::from_needs(&needs)
}

fn decoded(outputs: &[(&str, &str)]) -> Result<Option<HeldSlot>, NeedsError> {
    decoded_after("success", outputs)
}

/// What a reader of `admit` decodes after `held`'s outputs went through the `GITHUB_OUTPUT`
/// writer.
fn through_outputs(held: &HeldSlot) -> Option<HeldSlot> {
    let mut file = Vec::new();
    let mut outputs = Outputs::new(&mut file);
    for (name, value) in held.outputs() {
        outputs.write(name, &value).expect("single-line values");
    }
    let text = String::from_utf8(file).expect("UTF-8 outputs");
    let pairs: Vec<(&str, &str)> = text
        .lines()
        .map(|line| line.split_once('=').expect("name=value"))
        .collect();
    decoded(&pairs).expect("admit's own outputs decode")
}

#[test]
fn encodes_the_slot_index_and_the_lease_commit() {
    let outputs = held(1).outputs();

    assert_eq!(
        outputs,
        [
            (OutputName::Slot, "1".to_owned()),
            (OutputName::LeaseCommit, LEASE.to_owned()),
        ]
    );
}

#[test]
fn an_admission_that_took_no_slot_reads_as_none() {
    for outputs in [
        &[("admission", "no-spend")][..],
        &[
            ("admission", "deferred"),
            ("slot", ""),
            ("lease_commit", ""),
        ],
        &[],
    ] {
        assert_matches!(decoded(outputs), Ok(None), "{outputs:?}");
    }
}

#[test]
fn a_slot_is_read_whatever_admit_ended_with() {
    for result in ["success", "failure", "cancelled", "skipped"] {
        let read = decoded_after(result, &[("slot", "0"), ("lease_commit", LEASE)]);

        assert_eq!(read.ok(), Some(Some(held(0))), "{result}");
    }
}

#[test]
fn a_lease_commit_without_a_slot_is_refused() {
    let read = decoded(&[("lease_commit", LEASE)]);

    assert_matches!(
        read,
        Err(NeedsError::Output {
            job: JobName::Admit,
            name: OutputName::LeaseCommit
        })
    );
}

#[test]
fn a_slot_without_a_lease_commit_is_refused() {
    let read = decoded(&[("slot", "0")]);

    assert_matches!(
        read,
        Err(NeedsError::MissingOutput {
            job: JobName::Admit,
            name: OutputName::LeaseCommit
        })
    );
}

#[test]
fn a_slot_that_is_not_a_slot_index_is_refused() {
    for slot in ["2", "00", "01", "-0", "+1", " 1", "slot-1"] {
        let read = decoded(&[("slot", slot), ("lease_commit", LEASE)]);

        assert_matches!(
            read,
            Err(NeedsError::Output {
                job: JobName::Admit,
                name: OutputName::Slot
            }),
            "{slot:?}"
        );
    }
}

#[test]
fn a_lease_commit_that_is_not_a_full_commit_id_is_refused() {
    for lease in ["4444", &"A".repeat(40), &format!("{LEASE}0")] {
        let read = decoded(&[("slot", "1"), ("lease_commit", lease)]);

        assert_matches!(
            read,
            Err(NeedsError::Output {
                job: JobName::Admit,
                name: OutputName::LeaseCommit
            }),
            "{lease:?}"
        );
    }
}

#[test]
fn every_admit_output_is_accepted_and_any_other_is_refused() {
    let every: Vec<(&str, &str)> = OutputName::ADMIT
        .iter()
        .map(|name| (name.as_str(), ""))
        .collect();
    assert_matches!(decoded(&every), Ok(None));

    let read = decoded(&[("slot", "1"), ("lease_commit", LEASE), ("stage", "not-run")]);

    assert_matches!(
        read,
        Err(NeedsError::UnexpectedOutput { job: JobName::Admit, name }) if name == "stage"
    );
}

#[test]
fn a_missing_admit_is_refused() {
    let needs = Needs::parse(b"{}", &[]).expect("an empty needs");

    assert_matches!(
        HeldSlot::from_needs(&needs),
        Err(NeedsError::MissingJob {
            job: JobName::Admit
        })
    );
}

proptest! {
    #[test]
    fn decode_inverts_encode(index in 0_u8..2, lease in "[0-9a-f]{40}") {
        let held = HeldSlot {
            slot: slot(index),
            lease_commit: lease.parse().expect("40 lowercase hex digits"),
        };

        let read = through_outputs(&held);

        prop_assert_eq!(read, Some(held));
    }
}
