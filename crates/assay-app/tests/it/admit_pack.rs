//! `admit`'s context pack (04 §1 S1a, §3, §6, 05 T21, 03 §6d): the base suppress text, the
//! owner-valid dismissals from the ledger's command records, and the prior findings from the
//! previous run's bundle, with its digest; and the plan documents `admit` refuses.

use std::assert_matches;
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::num::NonZeroU64;

use assay_app::{
    AdmitError, Carry, CarryReset, CarrySet, CommandRecord, CommentId, CommentNow, EvidencePointer,
    LedgerPath, MAX_SUPPRESS_BYTES, PlanDocument, PriorCarryError, PushDraft, PushKind,
    Sha256Digest, UserId, append_command, append_push,
};
use assay_domain::{RunUid, Severity};
use serde_json::{Value, json};

use crate::admit_fakes::{AGENT, AUTHOR, NOW, OWNER, PR, REPO_ID, World, at, nz, plan, this_run};
use crate::publish_support::{Run, stamped};
use crate::replay_fakes::{FakeBundles, Stored};
use crate::support::{schema, schemas_dir};
use crate::verify_fakes::carried;

/// A privileged identity who pushed to the PR.
const PUSHER: u64 = 503;

/// The fingerprint every dismissal names.
const FP: &str = "0123456789abcdef";

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

/// Where `document` breaks `schemas/context-pack-1.schema.json`, as instance paths.
fn violations(document: &Value) -> Result<Vec<String>, Box<dyn Error>> {
    let validator = jsonschema::validator_for(&schema("context-pack-1")?)?;
    Ok(validator
        .iter_errors(document)
        .map(|error| error.instance_path().as_str().to_owned())
        .collect())
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
    assert_eq!(violations(&document)?, Vec::<String>::new());
    assert_eq!(
        (&document["schema"], &document["run_uid"]["run_id"]),
        (&json!("assay.context-pack/1"), &json!(9))
    );
    Ok(())
}

#[test]
fn the_context_pack_fixtures_pass_or_fail_their_schema() -> Result<(), Box<dyn Error>> {
    for (validity, expected_valid) in [("valid", true), ("invalid", false)] {
        let dir = schemas_dir()
            .join("fixtures")
            .join("context-pack")
            .join(validity);
        let mut seen = 0_usize;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let document: Value = serde_json::from_slice(&fs::read(&path)?)?;
            assert_eq!(
                violations(&document)?.is_empty(),
                expected_valid,
                "{}",
                path.display()
            );
            seen += 1;
        }
        assert!(seen > 0, "no {validity} fixture");
    }
    Ok(())
}

/// A comment `id` by `actor` saying `body`, recorded by decide-only, and as GitHub shows it now.
fn command(id: u64, actor: u64, body: &str) -> (CommandRecord, CommentNow) {
    let created = at("2026-10-19T10:00:00.000Z");
    let record = CommandRecord {
        run_uid: RunUid {
            repo_id: nz(REPO_ID),
            run_id: nz(100 + id),
            run_attempt: nz(1),
        },
        pr: nz(PR),
        comment_id: CommentId(nz(id)),
        actor_id: UserId(nz(actor)),
        command: body.to_owned(),
        body_sha256: Sha256Digest::of(body.as_bytes()),
        created_at: created,
        at: at("2026-10-19T10:00:05.000Z"),
    };
    let now = CommentNow {
        created_at: created,
        updated_at: created,
        body: body.to_owned(),
    };
    (record, now)
}

/// The ledger and comment store hold `dismiss` from the owner (comment 1) and from each identity
/// or comment state that voids it: the author (2), an agent (3), an edit (4), a deletion (5), a
/// changed body (6) and a pusher (7); and the owner's `/assay recheck` (8). Every one of them is
/// a privileged identity. The owner's opener record does not make the owner a pusher.
fn dismissals_of_every_kind(world: &mut World, dismiss: &str) -> Result<(), Box<dyn Error>> {
    world.identities.privileged =
        BTreeSet::from([OWNER, AUTHOR, AGENT, PUSHER].map(|id| UserId(nz(id))));
    append_push(
        &world.ledger,
        &PushDraft {
            run_uid: RunUid {
                run_id: nz(5),
                ..this_run(1)
            },
            pr: nz(PR),
            kind: PushKind::Opener,
            after: "a".repeat(40).parse()?,
            sender_id: UserId(nz(OWNER)),
            at: at("2026-10-19T08:00:00.000Z"),
        },
    )?;
    append_push(
        &world.ledger,
        &PushDraft {
            run_uid: this_run(1),
            pr: nz(PR),
            kind: PushKind::Push {
                before: "a".repeat(40).parse()?,
                forced: false,
            },
            after: "b".repeat(40).parse()?,
            sender_id: UserId(nz(PUSHER)),
            at: at("2026-10-19T09:00:00.000Z"),
        },
    )?;
    let cases = [
        command(1, OWNER, dismiss),
        command(2, AUTHOR, dismiss),
        command(3, AGENT, dismiss),
        command(4, OWNER, dismiss),
        command(5, OWNER, dismiss),
        command(6, OWNER, dismiss),
        command(7, PUSHER, dismiss),
        command(8, OWNER, "/assay recheck"),
    ];
    for (record, mut now) in cases {
        append_command(&world.ledger, &record)?;
        match record.comment_id.0.get() {
            // Edited after it was recorded.
            4 => now.updated_at = at("2026-10-19T10:30:00.000Z"),
            // Deleted.
            5 => continue,
            // Its body no longer hashes to the record.
            6 => now.body = format!("{dismiss}."),
            _ => {}
        }
        world
            .comments
            .comments
            .insert(record.comment_id.0.get(), now);
    }
    Ok(())
}

#[test]
fn only_an_eligible_owners_standing_dismissal_enters_the_pack() -> Result<(), Box<dyn Error>> {
    let dismiss = format!("/assay dismiss {FP} intended: the caller bounds it");
    let mut world = World::new();
    dismissals_of_every_kind(&mut world, &dismiss)?;

    let pack = pack_of(&mut world)?;

    assert_eq!(
        pack["dismissals"],
        json!([{
            "comment_id": 1,
            "actor_id": OWNER,
            "fp": FP,
            "reason": "intended: the caller bounds it",
        }])
    );
    assert_eq!(
        world
            .comments
            .asked
            .borrow()
            .iter()
            .map(|id| id.0.get())
            .collect::<Vec<_>>(),
        [1, 4, 5, 6],
        "only eligible dismiss commands are looked up"
    );
    Ok(())
}

#[test]
fn a_comment_store_that_cannot_be_read_fails_admit_before_the_slot() -> Result<(), Box<dyn Error>> {
    let mut world = World::new();
    let (record, _) = command(1, OWNER, &format!("/assay dismiss {FP} intended"));
    append_command(&world.ledger, &record)?;
    world.comments.down = true;

    let failed = world.admit(&plan("lite"));

    assert_matches!(failed, Err(AdmitError::Comments(_)));
    assert_eq!(world.refs_held(), Vec::<String>::new());
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
