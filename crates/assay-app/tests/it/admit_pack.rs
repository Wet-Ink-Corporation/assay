//! `admit`'s context pack (04 §1 S1a, §3, 03 §6d): the base suppress text and the prior findings
//! from the previous run's bundle, with the pack's digest and schema; and the plan documents
//! `admit` refuses. The dismissals are `admit_dismissals.rs`'.

use std::assert_matches;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::num::NonZeroU64;

use assay_app::{
    AdmitError, Carry, CarryReset, CarrySet, EvidencePointer, LedgerPath, MAX_SUPPRESS_BYTES,
    PlanDocument, PriorCarryError, Sha256Digest,
};
use assay_domain::{RunUid, Severity};
use serde_json::{Value, json};

use crate::admit_fakes::{NOW, REPO_ID, World, at, nz, plan, this_run};
use crate::publish_support::{Run, stamped};
use crate::replay_fakes::{FakeBundles, Stored};
use crate::support::{schema, schemas_dir};
use crate::verify_fakes::carried;

/// The pack `admit` built, parsed.
fn pack_of(world: &mut World) -> Result<Value, Box<dyn Error>> {
    let admitted = world.admit(&plan("lite"))?;
    let pack = admitted.pack().ok_or("no pack")?;
    Ok(serde_json::from_slice(pack.json())?)
}

/// A published run whose bundle carries one blocker: its bytes and the pointer to them.
fn published_carrying_one() -> Result<(Vec<u8>, EvidencePointer), Box<dyn Error>> {
    let mut run = Run::new()?;
    run.carry = CarrySet::unjudged(vec![carried(0, Severity::Blocker)?]);
    let world = stamped()?;
    run.publish(&world)?;
    let uploads = world.uploads.borrow();
    let [(_, _, bytes)] = uploads.as_slice() else {
        return Err(format!("expected one upload, got {}", uploads.len()).into());
    };
    let pointer = EvidencePointer {
        run_id: run.request.plan.run_uid.run_id,
        run_attempt: NonZeroU64::MIN,
        sha256: Sha256Digest::of(bytes),
    };
    // The bundle's bytes outlive the publishing world's upload log.
    Ok((bytes.clone(), pointer))
}

/// A world whose ledger points at a run with a stored bundle of `stored`, on `carry`.
fn after_a_run(carry: Carry, pointer: EvidencePointer, stored: Stored) -> World {
    World {
        bundles: FakeBundles::new(stored),
        carry,
        ..World::new()
    }
    .with_state(|state| state.evidence = Some(pointer))
}

/// Where `document` breaks `schemas/context-pack-1.schema.json`: instance path and keyword.
fn violations(document: &Value) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let validator = jsonschema::validator_for(&schema("context-pack-1")?)?;
    Ok(validator
        .iter_errors(document)
        .map(|error| {
            (
                error.instance_path().as_str().to_owned(),
                error.kind().keyword().to_owned(),
            )
        })
        .collect())
}

/// Each invalid fixture by file stem, with the one violation it must raise.
const INVALID: [(&str, &str, &str); 4] = [
    ("missing-suppress", "", "required"),
    ("prior-unknown-field", "/prior/0", "additionalProperties"),
    ("unknown-field", "", "additionalProperties"),
    ("uppercase-dismissed-fp", "/dismissals/0/fp", "pattern"),
];

/// The fixtures under `schemas/fixtures/context-pack/<validity>/`, by file stem.
fn fixtures(validity: &str) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    let dir = schemas_dir()
        .join("fixtures")
        .join("context-pack")
        .join(validity);
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("a fixture name")?
            .to_owned();
        found.insert(stem, serde_json::from_slice(&fs::read(&path)?)?);
    }
    Ok(found)
}

#[test]
fn a_kept_carry_brings_the_previous_runs_open_findings() -> Result<(), Box<dyn Error>> {
    let (bytes, pointer) = published_carrying_one()?;
    let previous = pointer.run_id;
    let mut world = after_a_run(Carry::Kept, pointer, Stored::Bytes(bytes));

    let pack = pack_of(&mut world)?;

    let prior = pack["prior"].as_array().ok_or("prior is a list")?;
    assert_eq!(
        prior
            .iter()
            .map(|finding| (&finding["fp"], &finding["severity"]))
            .collect::<Vec<_>>(),
        [(&json!("0000000000000000"), &json!("blocker"))]
    );
    assert_eq!(
        world.bundles.asked.borrow().first().map(|asked| asked.0),
        Some(RunUid {
            repo_id: nz(REPO_ID),
            run_id: previous,
            run_attempt: NonZeroU64::MIN,
        })
    );
    Ok(())
}

#[test]
fn the_pack_is_the_contract_and_its_digest_is_its_sha256() -> Result<(), Box<dyn Error>> {
    let (bytes, pointer) = published_carrying_one()?;
    let mut world = after_a_run(Carry::Kept, pointer, Stored::Bytes(bytes));
    world.suppress = Some("[[suppress]]\nrule = \"x\"\n".to_owned());

    let admitted = world.admit(&plan("lite"))?;

    let pack = admitted.pack().ok_or("no pack")?;
    assert_eq!(pack.digest(), &Sha256Digest::of(pack.json()));
    let document: Value = serde_json::from_slice(pack.json())?;
    assert_eq!(violations(&document)?, []);
    assert_eq!(
        (&document["schema"], &document["run_uid"]["run_id"]),
        (&json!("assay.context-pack/1"), &json!(9))
    );
    Ok(())
}

#[test]
fn every_valid_context_pack_fixture_passes_its_schema() -> Result<(), Box<dyn Error>> {
    let valid = fixtures("valid")?;

    assert!(!valid.is_empty(), "no valid fixture");
    for (stem, document) in valid {
        assert_eq!(violations(&document)?, [], "{stem}");
    }
    Ok(())
}

#[test]
fn each_invalid_context_pack_fixture_breaks_its_schema_where_it_says() -> Result<(), Box<dyn Error>>
{
    let invalid = fixtures("invalid")?;

    assert_eq!(
        invalid.keys().map(String::as_str).collect::<Vec<_>>(),
        INVALID.map(|(stem, _, _)| stem),
        "every invalid fixture has an entry, and every entry a fixture"
    );
    for (stem, pointer, keyword) in INVALID {
        let document = invalid.get(stem).ok_or(stem)?;
        assert_eq!(
            violations(document)?,
            [(pointer.to_owned(), keyword.to_owned())],
            "{stem}"
        );
    }
    Ok(())
}

#[test]
fn a_kept_carry_without_an_evidence_pointer_fails_admit() {
    let mut world = World {
        carry: Carry::Kept,
        ..World::new()
    };

    let failed = world.admit(&plan("lite"));

    assert_matches!(failed, Err(AdmitError::NoPriorPointer));
    assert_eq!(world.refs_held(), Vec::<String>::new());
}

#[test]
fn a_missing_prior_bundle_fails_admit_with_nothing_taken() -> Result<(), Box<dyn Error>> {
    let (_, pointer) = published_carrying_one()?;
    let mut world = after_a_run(Carry::Kept, pointer, Stored::Missing);

    let failed = world.admit(&plan("lite"));

    assert_matches!(failed, Err(AdmitError::Prior(PriorCarryError::Missing)));
    assert_eq!(world.refs_held(), Vec::<String>::new());
    assert_eq!(world.ledger.file(&LedgerPath::costs(at(NOW).month())), None);
    Ok(())
}

#[test]
fn a_pointer_other_than_the_one_the_carry_was_decided_on_fails_admit() -> Result<(), Box<dyn Error>>
{
    let (bytes, pointer) = published_carrying_one()?;
    let other = EvidencePointer {
        sha256: Sha256Digest::of(b"another bundle"),
        ..pointer
    };
    for decided_on in [None, Some(other)] {
        let mut world = after_a_run(Carry::Kept, pointer.clone(), Stored::Bytes(bytes.clone()));
        // A run completed between the caller's read of the state and admit's.
        world.prior = decided_on;

        let failed = world.admit(&plan("lite"));

        assert_matches!(failed, Err(AdmitError::PriorMoved));
        assert_eq!(world.refs_held(), Vec::<String>::new());
        assert_eq!(world.ledger.file(&LedgerPath::costs(at(NOW).month())), None);
        assert_eq!(world.bundles.asked.borrow().len(), 0, "no bundle is read");
    }
    Ok(())
}

#[test]
fn a_reset_carry_reads_no_bundle() -> Result<(), Box<dyn Error>> {
    let (_, pointer) = published_carrying_one()?;
    let mut world = after_a_run(Carry::Reset(CarryReset::Rewritten), pointer, Stored::Fails);

    let pack = pack_of(&mut world)?;

    assert_eq!(pack["prior"], json!([]));
    assert_eq!(world.bundles.asked.borrow().len(), 0);
    Ok(())
}

#[test]
fn the_suppress_text_is_carried_verbatim_and_null_without_a_file() -> Result<(), Box<dyn Error>> {
    let text = "[[suppress]]\nrule = \"clippy::too_many_lines\"\n";
    let mut with_file = World {
        suppress: Some(text.to_owned()),
        ..World::new()
    };
    let mut without = World::new();

    assert_eq!(pack_of(&mut with_file)?["suppress"], json!(text));
    assert_eq!(pack_of(&mut without)?["suppress"], Value::Null);
    Ok(())
}

#[test]
fn a_suppress_file_over_its_cap_fails_admit() {
    let mut world = World {
        suppress: Some("#".repeat(MAX_SUPPRESS_BYTES + 1)),
        ..World::new()
    };

    let failed = world.admit(&plan("lite"));

    assert_matches!(
        failed,
        Err(AdmitError::SuppressTooLarge { size }) if size == MAX_SUPPRESS_BYTES + 1
    );
    assert_eq!(world.refs_held(), Vec::<String>::new());
}

#[test]
fn a_plan_that_already_carries_a_budget_is_refused() -> Result<(), Box<dyn Error>> {
    let admitted = World::new().admit(&plan("lite"))?;
    let document: Value = serde_json::from_slice(admitted.run_json())?;
    let mut budgeted = plan("lite");
    budgeted.budget = document.get("budget").cloned();
    let mut world = World::new();

    let refused = world.admit(&budgeted);

    assert_matches!(refused, Err(AdmitError::Plan(PlanDocument::Budgeted)));
    assert_eq!(world.refs_held(), Vec::<String>::new());
    Ok(())
}

#[test]
fn a_plan_of_another_run_is_refused() {
    let mut other = plan("lite");
    other.run_uid = RunUid {
        run_id: nz(10),
        ..this_run(1)
    };
    let mut world = World::new();

    let refused = world.admit(&other);

    assert_matches!(
        refused,
        Err(AdmitError::Plan(PlanDocument::OtherRun { planned })) if planned.run_id.get() == 10
    );
}
