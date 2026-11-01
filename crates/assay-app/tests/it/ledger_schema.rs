//! The ledger record schemas (`schemas/ledger-*-1.schema.json`) and the strict reader agree on
//! the golden fixtures: each valid one passes both, each invalid one fails the schema where
//! listed and is refused by the reader; and every record the writer produces passes its schema.
//! The lease contracts (`ledger-lease`, `ledger-lock`, `ledger-grant`, #71) are commit messages
//! of lease refs rather than files, checked the same way.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;

use assay_app::{
    CommandRecord, CommentId, CostKind, GrantKey, LeaseRef, LedgerPath, LockRequest, PrState,
    PushDraft, PushKind, RetryGrant, Role, Slot, UserId, acquire_lock, acquire_slot,
    append_command, append_cost, append_push, consume_grant, grant_retry, read_commands,
    read_costs, read_pushes, read_slots, read_state, update_state,
};
use serde_json::Value;

use crate::lease_fakes::{MemoryRefs, slot_request};
use crate::ledger_fakes::{MemoryLedger, at, commit, cost, digest, id, run};
use crate::support::{schema, schemas_dir};

/// Schema violations as (instance path, keyword).
type Violations = Vec<(String, String)>;

/// Where `document` breaks `schemas/<stem>.schema.json`.
fn violations(stem: &str, document: &Value) -> Result<Violations, Box<dyn Error>> {
    let validator = jsonschema::validator_for(&schema(stem)?)?;
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

/// Golden fixtures by file stem: each file's bytes, one JSON line in the form assay writes, and
/// the document they parse to.
type Fixtures = BTreeMap<String, (Vec<u8>, Value)>;

/// The fixtures of `contract` (`ledger-cost`, ...) with `validity`.
fn fixtures(contract: &str, validity: &str) -> Result<Fixtures, Box<dyn Error>> {
    let dir = schemas_dir().join("fixtures").join(contract).join(validity);
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("a fixture name")?
            .to_owned();
        let bytes = fs::read(&path)?;
        let document = serde_json::from_slice(&bytes)?;
        found.insert(stem, (bytes, document));
    }
    Ok(found)
}

/// Whether the reader of `contract` accepts the file `bytes` as is, as the only record of PR 7
/// of repository 42 (cost records in October 2026's file).
fn reader_accepts(contract: &str, bytes: &[u8]) -> Result<bool, Box<dyn Error>> {
    let october = at("2026-10-15T00:00:00.000Z");
    let accepted = match contract {
        "ledger-cost" => {
            let ledger =
                MemoryLedger::default().with_file(&LedgerPath::costs(october.month()), bytes);
            read_costs(&ledger, october).is_ok_and(|window| window.records().len() == 1)
        }
        "ledger-push" => {
            let ledger =
                MemoryLedger::default().with_file(&LedgerPath::pushes(id(42), id(7)), bytes);
            read_pushes(&ledger, id(42), id(7)).is_ok_and(|records| records.len() == 1)
        }
        "ledger-command" => {
            let ledger =
                MemoryLedger::default().with_file(&LedgerPath::commands(id(42), id(7)), bytes);
            read_commands(&ledger, id(42), id(7)).is_ok_and(|records| records.len() == 1)
        }
        "ledger-state" => {
            let ledger =
                MemoryLedger::default().with_file(&LedgerPath::state(id(42), id(7)), bytes);
            read_state(&ledger, id(42), id(7)).is_ok()
        }
        "ledger-lease" | "ledger-lock" | "ledger-grant" => lease_reader_accepts(contract, bytes)?,
        other => return Err(format!("no reader for {other}").into()),
    };
    Ok(accepted)
}

/// Whether the lease reader of `contract` accepts `bytes` as the message of its ref: slot-0, the
/// lock of head `bbbb…` in repository 42, or the `security` grant of PR 7 on that head.
fn lease_reader_accepts(contract: &str, bytes: &[u8]) -> Result<bool, Box<dyn Error>> {
    let message = std::str::from_utf8(bytes)?;
    let refs = MemoryRefs::default();
    let before = at("2026-10-12T09:00:00.000Z");
    Ok(match contract {
        "ledger-lease" => {
            let slot = Slot::new(0).ok_or("slot 0")?;
            refs.holding(LeaseRef::slot(slot).as_str(), message);
            read_slots(&refs, before).is_ok_and(|views| views.len() == 1)
        }
        "ledger-lock" => {
            refs.holding(
                LeaseRef::publish_lock(id(42), &commit('b')).as_str(),
                message,
            );
            let request = LockRequest {
                run_uid: run(1),
                repo: "acme/widgets".to_owned(),
                head: commit('b'),
            };
            acquire_lock(&refs, &request, before).is_ok()
        }
        _ => {
            let key = GrantKey {
                repo_id: id(42),
                pr: id(7),
                head_sha: commit('b'),
                role: Role::Security,
            };
            refs.holding(
                LeaseRef::retry_grant(key.repo_id, key.pr, &key.head_sha, key.role).as_str(),
                message,
            );
            consume_grant(&refs, &key)
                .is_ok_and(|consumed| consumed != assay_app::Consumed::NoGrant)
        }
    })
}

/// Checks every fixture of `contract`: valid ones pass schema and reader; invalid ones fail the
/// schema exactly as `expected` lists, by stem, and are refused by the reader.
fn check(contract: &str, expected: &[(&str, &str, &str)]) -> Result<(), Box<dyn Error>> {
    let stem = format!("{contract}-1");
    for (name, (bytes, document)) in fixtures(contract, "valid")? {
        assert_eq!(violations(&stem, &document)?, [], "{contract} valid/{name}");
        assert!(
            reader_accepts(contract, &bytes)?,
            "{contract} valid/{name} read"
        );
    }
    let mut rejected = Vec::new();
    for (name, (bytes, document)) in fixtures(contract, "invalid")? {
        assert!(
            !reader_accepts(contract, &bytes)?,
            "{contract} invalid/{name} must be refused by the reader"
        );
        rejected.push((name, violations(&stem, &document)?));
    }
    let listed: Vec<(String, Violations)> = expected
        .iter()
        .map(|(name, path, keyword)| {
            (
                (*name).to_owned(),
                vec![((*path).to_owned(), (*keyword).to_owned())],
            )
        })
        .collect();
    assert_eq!(rejected, listed, "{contract} invalid fixtures");
    Ok(())
}

#[test]
fn the_cost_fixtures_pass_or_fail_schema_and_reader_alike() -> Result<(), Box<dyn Error>> {
    check(
        "ledger-cost",
        &[
            ("cancelled-reservation", "/cancelled", "const"),
            ("dot-repository", "/repo", "not"),
            ("inexact-aeu", "/aeu", "pattern"),
            ("second-precision", "/at", "pattern"),
            ("trivial-tier", "/tier", "enum"),
            ("unknown-field", "", "additionalProperties"),
            ("unknown-role", "/roles/0/role", "enum"),
            ("unknown-version", "/schema", "const"),
        ],
    )
}

#[test]
fn the_push_fixtures_pass_or_fail_schema_and_reader_alike() -> Result<(), Box<dyn Error>> {
    check(
        "ledger-push",
        &[
            ("forced-opener", "/forced", "const"),
            ("opener-with-before", "", "not"),
            ("push-without-before", "", "required"),
            ("short-after", "/after", "pattern"),
            ("unknown-field", "", "additionalProperties"),
            ("unknown-kind", "/kind", "enum"),
            ("zero-seq", "/seq", "minimum"),
        ],
    )
}

#[test]
fn append_command_writes_the_golden_line() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();
    let golden = fs::read(
        schemas_dir()
            .join("fixtures")
            .join("ledger-command")
            .join("valid")
            .join("command.json"),
    )?;

    append_command(
        &ledger,
        &CommandRecord {
            run_uid: run(7003),
            pr: id(7),
            comment_id: CommentId(id(900_001)),
            actor_id: UserId(id(1001)),
            head_sha: commit('b'),
            command: "/assay waive 0123456789abcdef".to_owned(),
            body_sha256: digest('c'),
            created_at: at("2026-10-12T09:29:58.000Z"),
            at: at("2026-10-12T09:30:00.000Z"),
        },
    )?;

    assert_eq!(
        ledger.text(&LedgerPath::commands(id(42), id(7))),
        String::from_utf8(golden)?
    );
    Ok(())
}

#[test]
fn the_command_fixtures_pass_or_fail_schema_and_reader_alike() -> Result<(), Box<dyn Error>> {
    check(
        "ledger-command",
        &[
            ("control-character", "/command", "pattern"),
            ("long-command", "/command", "maxLength"),
            ("missing-body-hash", "", "required"),
            ("missing-head-sha", "", "required"),
            ("recheck", "/command", "pattern"),
            ("unknown-word", "/command", "pattern"),
        ],
    )
}

#[test]
fn the_state_fixtures_pass_or_fail_schema_and_reader_alike() -> Result<(), Box<dyn Error>> {
    check(
        "ledger-state",
        &[
            (
                "deferral-unknown-field",
                "/deferral",
                "additionalProperties",
            ),
            ("missing-counters", "", "required"),
            ("published-pending-key", "/published/key", "pattern"),
            (
                "published-unknown-field",
                "/published",
                "additionalProperties",
            ),
            ("unknown-llm", "/evidence_records/0/llm", "enum"),
        ],
    )
}

#[test]
fn the_lease_fixtures_pass_or_fail_schema_and_reader_alike() -> Result<(), Box<dyn Error>> {
    check(
        "ledger-lease",
        &[
            ("inexact-aeu", "/reserved_aeu", "pattern"),
            ("second-precision", "/expires_at", "pattern"),
            ("unknown-field", "", "additionalProperties"),
            ("unknown-version", "/schema", "const"),
        ],
    )
}

#[test]
fn the_lock_fixtures_pass_or_fail_schema_and_reader_alike() -> Result<(), Box<dyn Error>> {
    check(
        "ledger-lock",
        &[
            ("missing-expiry", "", "required"),
            ("short-head", "/head_sha", "pattern"),
            ("unknown-field", "", "additionalProperties"),
        ],
    )
}

#[test]
fn the_grant_fixtures_pass_or_fail_schema_and_reader_alike() -> Result<(), Box<dyn Error>> {
    check(
        "ledger-grant",
        &[
            ("unknown-field", "", "additionalProperties"),
            ("unknown-role", "/role", "enum"),
            ("zero-pr", "/pr", "minimum"),
        ],
    )
}

#[test]
fn every_lease_the_writer_produces_passes_its_schema() -> Result<(), Box<dyn Error>> {
    let refs = MemoryRefs::default();
    let now = at("2026-10-12T09:30:00.000Z");
    acquire_slot(&refs, &slot_request(1), now)?;
    let lock = LockRequest {
        run_uid: run(2),
        repo: "acme/widgets".to_owned(),
        head: commit('b'),
    };
    acquire_lock(&refs, &lock, now)?;
    let grant = RetryGrant {
        run_uid: run(3),
        repo: "acme/widgets".to_owned(),
        pr: id(7),
        head_sha: commit('b'),
        role: Role::CombinedMicro,
        at: now,
    };
    grant_retry(&refs, &grant)?;

    let mut checked = 0_usize;
    for name in refs.names() {
        let stem = match name.split('/').nth(2) {
            Some(leaf) if leaf.starts_with("slot-") => "ledger-lease-1",
            Some(leaf) if leaf.starts_with("lock-") => "ledger-lock-1",
            Some("retry") => "ledger-grant-1",
            _ => return Err(format!("unexpected ref {name}").into()),
        };
        let (_, message) = refs.get(&name).ok_or("the ref was listed")?;
        let document: Value = serde_json::from_str(&message)?;
        assert_eq!(violations(stem, &document)?, [], "{stem}: {document}");
        checked += 1;
    }
    assert_eq!(checked, 3, "a slot lease, a lock and a grant");
    Ok(())
}

/// Each line of `text` as JSON.
fn documents(text: &str) -> Result<Vec<Value>, Box<dyn Error>> {
    Ok(text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

/// Writes a reservation and its cancelled settlement, an opener and a push, a command and a
/// state.
fn write_one_of_each(ledger: &MemoryLedger) -> Result<(), Box<dyn Error>> {
    let mut settled = cost(1, CostKind::Final, 2_300_000, "2026-10-12T09:41:00.000Z");
    settled.cancelled = true;
    append_cost(
        ledger,
        &cost(1, CostKind::Reserved, 2_300_000, "2026-10-12T09:30:00.000Z"),
    )?;
    append_cost(ledger, &settled)?;
    append_push(
        ledger,
        &PushDraft {
            run_uid: run(5),
            pr: id(7),
            kind: PushKind::Opener,
            after: commit('a'),
            sender_id: UserId(id(1001)),
            at: at("2026-10-12T09:20:00.000Z"),
        },
    )?;
    append_push(
        ledger,
        &PushDraft {
            run_uid: run(2),
            pr: id(7),
            kind: PushKind::Push {
                before: commit('a'),
                forced: true,
            },
            after: commit('b'),
            sender_id: UserId(id(1001)),
            at: at("2026-10-12T09:30:00.000Z"),
        },
    )?;
    append_command(
        ledger,
        &CommandRecord {
            run_uid: run(3),
            pr: id(7),
            comment_id: CommentId(id(900_001)),
            actor_id: UserId(id(1001)),
            head_sha: commit('b'),
            command: "/assay waive 0123456789abcdef".to_owned(),
            body_sha256: digest('c'),
            created_at: at("2026-10-12T09:29:58.000Z"),
            at: at("2026-10-12T09:30:00.000Z"),
        },
    )?;
    update_state(ledger, id(42), id(7), |_state| {
        Some(PrState {
            decision: Some(assay_app::DecisionPointer {
                run_id: id(4),
                sha256: digest('d'),
            }),
            ..PrState::default()
        })
    })?;
    Ok(())
}

#[test]
fn every_record_the_writer_produces_passes_its_schema() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    write_one_of_each(&ledger)?;

    let written = [
        (
            "ledger-cost-1",
            LedgerPath::costs(at("2026-10-12T09:30:00.000Z").month()),
        ),
        ("ledger-push-1", LedgerPath::pushes(id(42), id(7))),
        ("ledger-command-1", LedgerPath::commands(id(42), id(7))),
        ("ledger-state-1", LedgerPath::state(id(42), id(7))),
    ];
    let mut checked = 0_usize;
    for (stem, path) in written {
        for document in documents(&ledger.text(&path))? {
            assert_eq!(violations(stem, &document)?, [], "{stem}: {document}");
            checked += 1;
        }
    }
    assert_eq!(
        checked, 6,
        "two cost records, an opener and a push, one command, one state"
    );
    Ok(())
}
