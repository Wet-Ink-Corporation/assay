//! The previous completed run's head (#60 ST-11, D6 A): the ledger's evidence pointer names no
//! head, so it is read from the bundle the pointer authenticates, as its `github.head_sha`.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    BundleShape, CommitId, EvidencePointer, PriorCarry, PriorCarryError, Sha256Digest, prior_head,
};
use serde_json::{Value, json};

use crate::plan_fakes::FakeError;
use crate::publish_support::Run;
use crate::replay_fakes::{FakeBundles, Stored};
use crate::rereview_carry::{published, two_carried};
use crate::verify_fakes::REPOSITORY;

/// What `prior_head` reads for `run`'s repository through `pointer` from `stored`.
fn head_of(
    run: &Run,
    pointer: &EvidencePointer,
    stored: Stored,
) -> Result<CommitId, PriorCarryError<FakeError>> {
    let request = PriorCarry {
        repo_id: run.request.plan.run_uid.repo_id,
        pointer,
        repository: REPOSITORY,
    };
    prior_head(&request, &FakeBundles::new(stored))
}

#[test]
fn the_previous_head_is_the_bundle_s_snapshot_head() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = published(two_carried()?)?;
    let snapshot: Value = serde_json::from_slice(&bytes)?;
    let recorded = snapshot["github"]["head_sha"]
        .as_str()
        .ok_or("the bundle records its head")?
        .to_owned();

    let head = head_of(&run, &pointer, Stored::Bytes(bytes))?;

    assert_eq!(head.as_str(), recorded);
    Ok(())
}

#[test]
fn a_bundle_whose_head_is_not_a_commit_id_gives_no_previous_head() -> Result<(), Box<dyn Error>> {
    let (run, bytes, mut pointer) = published(two_carried()?)?;
    let mut snapshot: Value = serde_json::from_slice(&bytes)?;
    snapshot["github"]["head_sha"] = json!("not a commit");
    let bytes = serde_json::to_vec(&snapshot)?;
    pointer.sha256 = Sha256Digest::of(&bytes);

    let head = head_of(&run, &pointer, Stored::Bytes(bytes));

    assert_matches!(head, Err(PriorCarryError::Shape(BundleShape::Shape)));
    Ok(())
}

#[test]
fn the_previous_head_is_read_only_from_a_bundle_that_hashes_to_its_pointer()
-> Result<(), Box<dyn Error>> {
    let (run, mut bytes, pointer) = published(two_carried()?)?;
    bytes.push(b' ');

    let head = head_of(&run, &pointer, Stored::Bytes(bytes));

    assert_matches!(head, Err(PriorCarryError::Digest));
    Ok(())
}
