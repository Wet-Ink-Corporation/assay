//! The ledger repository adapter against documented contents API responses (`ledger_*.json`
//! follow GitHub's REST schema for `GET` and `PUT /repos/{owner}/{repo}/contents/{path}` and
//! `GET /repos/{owner}/{repo}/git/blobs/{sha}`; the ledger repository is App-only, so nothing
//! here was recorded from it), and against an in-memory contents API for concurrent appends.

use std::time::Duration;

use assay_app::{
    Appended, FileVersion, Ledger, LedgerFile, MAX_LEDGER_FILE_BYTES, WriteCondition, WriteOutcome,
    append_cost, read_costs,
};
use assay_domain::Aeu;
use assay_forge::github::{ForgeError, LedgerRepoError, MAX_RESPONSE_BYTES, Method, Operation};
use serde_json::{Value, json};

use super::contents::{
    ContentsApi, LEDGER_REPOSITORY, at, contents_path, encode, ledger, october, reservation,
};
use super::recorded::{Recorded, Unscripted, Waits, fixture, fixture_json, json, reply};

/// The version `ledger_contents.json` gives the file.
const SHA: &str = "3d21ec53a331a6f037a91c368710b99387d012c1";

#[test]
fn a_file_is_read_with_its_version_from_the_contents_api() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &contents_path(&october()),
        fixture("ledger_contents.json"),
    );
    let waits = Waits::default();

    let file = ledger(&recorded, &waits).read(&october());

    assert_eq!(
        file.ok(),
        Some(Some(LedgerFile {
            version: FileVersion::new(SHA),
            contents: fixture("ledger_line.jsonl"),
        }))
    );
}

fn not_found() -> Recorded {
    Recorded::default().then(
        Method::Get,
        &contents_path(&october()),
        reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
    )
}

#[test]
fn an_absent_file_of_a_visible_repository_reads_as_none() {
    let recorded = not_found().ok(
        Method::Get,
        &format!("repos/{LEDGER_REPOSITORY}"),
        fixture("ledger_repository.json"),
    );
    let waits = Waits::default();

    let file = ledger(&recorded, &waits).read(&october());

    assert_eq!(file.ok(), Some(None));
}

#[test]
fn a_404_from_a_repository_the_token_cannot_see_is_an_error_not_an_empty_file() {
    let recorded = not_found().then(
        Method::Get,
        &format!("repos/{LEDGER_REPOSITORY}"),
        reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
    );
    let waits = Waits::default();

    let file = ledger(&recorded, &waits).read(&october());

    assert!(
        matches!(
            file,
            Err(LedgerRepoError::Forge(ForgeError::Rejected {
                status: 404,
                operation: Operation::ReadLedgerFile
            }))
        ),
        "{file:?}"
    );
}

#[test]
fn a_file_over_one_megabyte_is_read_through_its_blob() {
    let blob_sha = "8f9e2b0c41f6a2a7d1f0c9e3b5a4d6c7e8f90123";
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &contents_path(&october()),
            fixture("ledger_contents_none.json"),
        )
        .ok(
            Method::Get,
            &format!("repos/{LEDGER_REPOSITORY}/git/blobs/{blob_sha}"),
            fixture("ledger_blob.json"),
        );
    let waits = Waits::default();

    let file = ledger(&recorded, &waits).read(&october());

    assert_eq!(
        file.ok(),
        Some(Some(LedgerFile {
            version: FileVersion::new(blob_sha),
            contents: fixture("ledger_line.jsonl"),
        }))
    );
}

#[test]
fn the_largest_ledger_file_reads_through_its_blob_within_the_response_cap() {
    let contents = vec![b'x'; MAX_LEDGER_FILE_BYTES];
    let encoded = encode(&contents);
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(60)
        .map(|line| std::str::from_utf8(line).expect("base64 is ASCII"))
        .collect();
    let mut listing = fixture_json("ledger_contents_none.json");
    listing["size"] = json!(MAX_LEDGER_FILE_BYTES);
    let mut blob = fixture_json("ledger_blob.json");
    blob["size"] = json!(MAX_LEDGER_FILE_BYTES);
    blob["content"] = json!(format!("{}\n", lines.join("\n")));
    let blob = json(&blob);
    assert!(blob.len() <= MAX_RESPONSE_BYTES, "{} bytes", blob.len());
    let blob_sha = "8f9e2b0c41f6a2a7d1f0c9e3b5a4d6c7e8f90123";
    let recorded = Recorded::default()
        .ok(Method::Get, &contents_path(&october()), json(&listing))
        .ok(
            Method::Get,
            &format!("repos/{LEDGER_REPOSITORY}/git/blobs/{blob_sha}"),
            blob,
        );
    let waits = Waits::default();

    let file = ledger(&recorded, &waits).read(&october());

    assert_eq!(
        file.ok().flatten().map(|file| file.contents),
        Some(contents)
    );
}

/// Reading `october()` answered with `body` fails as `check` says, after no other request.
fn read_refused(body: &Value, check: impl Fn(&LedgerRepoError<Unscripted>) -> bool) {
    let recorded = Recorded::default().ok(Method::Get, &contents_path(&october()), json(body));
    let waits = Waits::default();

    let file = ledger(&recorded, &waits).read(&october());

    assert!(matches!(&file, Err(error) if check(error)), "{file:?}");
    assert_eq!(recorded.unused(), 0, "nothing else was asked for");
    assert_eq!(recorded.sent().len(), 1, "no blob was fetched");
}

#[test]
fn a_file_over_the_cap_is_refused_before_it_is_fetched_or_decoded() {
    let mut body = fixture_json("ledger_contents_none.json");
    body["size"] = json!(MAX_LEDGER_FILE_BYTES + 1);

    read_refused(
        &body,
        |error| matches!(error, LedgerRepoError::TooLarge { size, .. } if *size == u64::try_from(MAX_LEDGER_FILE_BYTES + 1).expect("fits")),
    );
}

#[test]
fn a_directory_is_refused() {
    read_refused(
        &json!([{ "type": "file", "path": "ledger/2026-10.jsonl" }]),
        |error| {
            matches!(
                error,
                LedgerRepoError::Forge(ForgeError::Json {
                    operation: Operation::ReadLedgerFile,
                    ..
                })
            )
        },
    );
}

#[test]
fn a_file_at_another_path_is_refused() {
    let mut body = fixture_json("ledger_contents.json");
    body["path"] = json!("ledger/2026-09.jsonl");

    read_refused(&body, |error| {
        matches!(
            error,
            LedgerRepoError::Forge(ForgeError::Malformed {
                operation: Operation::ReadLedgerFile,
                ..
            })
        )
    });
}

#[test]
fn a_size_that_disagrees_with_the_content_is_refused() {
    let mut body = fixture_json("ledger_contents.json");
    body["size"] = json!(3);

    read_refused(&body, |error| {
        matches!(error, LedgerRepoError::Forge(ForgeError::Malformed { .. }))
    });
}

#[test]
fn content_that_is_not_base64_is_refused() {
    let mut body = fixture_json("ledger_contents.json");
    body["content"] = json!("not base64!");

    read_refused(&body, |error| {
        matches!(error, LedgerRepoError::Forge(ForgeError::Malformed { .. }))
    });
}

#[test]
fn a_symlink_is_refused() {
    let mut body = fixture_json("ledger_contents.json");
    body["type"] = json!("symlink");

    read_refused(&body, |error| {
        matches!(error, LedgerRepoError::Forge(ForgeError::Malformed { .. }))
    });
}

#[test]
fn a_write_at_a_version_sends_its_sha_and_base64_contents() {
    let recorded = Recorded::default().ok(
        Method::Put,
        &contents_path(&october()),
        fixture("ledger_put.json"),
    );
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(
        &october(),
        b"foobar",
        &WriteCondition::At(FileVersion::new(SHA)),
    );

    assert_eq!(outcome.ok(), Some(WriteOutcome::Written));
    assert_eq!(
        recorded
            .writes()
            .into_iter()
            .map(|sent| sent.body)
            .collect::<Vec<_>>(),
        [Some(json!({
            "message": "assay-ledger: update ledger/2026-10.jsonl",
            "content": "Zm9vYmFy",
            "sha": SHA,
        }))]
    );
}

#[test]
fn creating_a_file_sends_no_sha() {
    let recorded = Recorded::default().then(
        Method::Put,
        &contents_path(&october()),
        reply(201, &[], fixture("ledger_put.json")),
    );
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(&october(), b"foo", &WriteCondition::Absent);

    assert_eq!(outcome.ok(), Some(WriteOutcome::Written));
    assert_eq!(
        recorded
            .writes()
            .into_iter()
            .map(|sent| sent.body)
            .collect::<Vec<_>>(),
        [Some(json!({
            "message": "assay-ledger: update ledger/2026-10.jsonl",
            "content": "Zm9v",
        }))]
    );
}

/// Writing `october()` under `condition`, answered `status` with `body`.
fn written(
    condition: &WriteCondition,
    status: u16,
    body: &str,
) -> Result<WriteOutcome, LedgerRepoError<Unscripted>> {
    let recorded = Recorded::default().then(
        Method::Put,
        &contents_path(&october()),
        reply(status, &[], fixture(body)),
    );
    let waits = Waits::default();
    ledger(&recorded, &waits).write(&october(), b"foo", condition)
}

#[test]
fn a_stale_version_is_a_conflict() {
    let outcome = written(
        &WriteCondition::At(FileVersion::new(SHA)),
        409,
        "ledger_put_conflict.json",
    );

    assert_eq!(outcome.ok(), Some(WriteOutcome::Conflict));
}

#[test]
fn creating_a_file_that_exists_is_a_conflict() {
    let outcome = written(&WriteCondition::Absent, 422, "ledger_put_no_sha.json");

    assert_eq!(outcome.ok(), Some(WriteOutcome::Conflict));
}

#[test]
fn a_422_on_an_update_is_a_rejection() {
    let outcome = written(
        &WriteCondition::At(FileVersion::new(SHA)),
        422,
        "ledger_put_no_sha.json",
    );

    assert!(
        matches!(
            outcome,
            Err(LedgerRepoError::Forge(ForgeError::Rejected {
                status: 422,
                operation: Operation::WriteLedgerFile
            }))
        ),
        "{outcome:?}"
    );
}

#[test]
fn a_write_over_the_cap_is_refused_unsent() {
    let recorded = Recorded::default();
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(
        &october(),
        &vec![b'x'; MAX_LEDGER_FILE_BYTES + 1],
        &WriteCondition::Absent,
    );

    assert!(
        matches!(outcome, Err(LedgerRepoError::TooLarge { .. })),
        "{outcome:?}"
    );
    assert_eq!(recorded.sent(), []);
}

#[test]
fn a_failed_write_that_landed_is_not_sent_again() {
    let mut landed = fixture_json("ledger_contents.json");
    landed["sha"] = json!("95b966ae1c166bd92f8ae7d1c313e738c731dfc3");
    landed["content"] = json!("Zm9v\n");
    landed["size"] = json!(3);
    let recorded = Recorded::default()
        .then(
            Method::Put,
            &contents_path(&october()),
            reply(502, &[], Vec::new()),
        )
        .ok(Method::Get, &contents_path(&october()), json(&landed));
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(
        &october(),
        b"foo",
        &WriteCondition::At(FileVersion::new(SHA)),
    );

    assert_eq!(outcome.ok(), Some(WriteOutcome::Written));
    assert_eq!(recorded.writes().len(), 1);
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
}

#[test]
fn a_failed_write_that_did_not_land_is_sent_again() {
    let recorded = Recorded::default()
        .then(
            Method::Put,
            &contents_path(&october()),
            reply(502, &[], Vec::new()),
        )
        .ok(
            Method::Get,
            &contents_path(&october()),
            fixture("ledger_contents.json"),
        )
        .ok(
            Method::Put,
            &contents_path(&october()),
            fixture("ledger_put.json"),
        );
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(
        &october(),
        b"foo",
        &WriteCondition::At(FileVersion::new(SHA)),
    );

    assert_eq!(outcome.ok(), Some(WriteOutcome::Written));
    assert_eq!(recorded.writes().len(), 2);
}

/// `outcome` is `Unconfirmed` for `october()`: the write may or may not have landed.
fn assert_unconfirmed(outcome: &Result<WriteOutcome, LedgerRepoError<Unscripted>>) {
    assert!(
        matches!(outcome, Err(LedgerRepoError::Unconfirmed { path }) if *path == october()),
        "{outcome:?}"
    );
}

#[test]
fn a_failed_write_after_which_the_file_moved_is_unconfirmed() {
    let mut moved = fixture_json("ledger_contents.json");
    moved["sha"] = json!("1acc419d4d6a9ce985db7be48c6349a0475975b5");
    let recorded = Recorded::default()
        .then(
            Method::Put,
            &contents_path(&october()),
            reply(503, &[], Vec::new()),
        )
        .ok(Method::Get, &contents_path(&october()), json(&moved));
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(
        &october(),
        b"foo",
        &WriteCondition::At(FileVersion::new(SHA)),
    );

    assert_unconfirmed(&outcome);
    assert_eq!(recorded.writes().len(), 1);
}

#[test]
fn a_resend_refused_as_stale_after_a_failed_write_is_unconfirmed() {
    let recorded = Recorded::default()
        .then(
            Method::Put,
            &contents_path(&october()),
            reply(502, &[], Vec::new()),
        )
        .ok(
            Method::Get,
            &contents_path(&october()),
            fixture("ledger_contents.json"),
        )
        .then(
            Method::Put,
            &contents_path(&october()),
            reply(409, &[], fixture("ledger_put_conflict.json")),
        );
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(
        &october(),
        b"foo",
        &WriteCondition::At(FileVersion::new(SHA)),
    );

    assert_unconfirmed(&outcome);
    assert_eq!(recorded.writes().len(), 2);
}

#[test]
fn a_create_resend_refused_after_a_failed_write_is_unconfirmed() {
    let recorded = Recorded::default()
        .then(
            Method::Put,
            &contents_path(&october()),
            reply(502, &[], Vec::new()),
        )
        .then(
            Method::Get,
            &contents_path(&october()),
            reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
        )
        .ok(
            Method::Get,
            &format!("repos/{LEDGER_REPOSITORY}"),
            fixture("ledger_repository.json"),
        )
        .then(
            Method::Put,
            &contents_path(&october()),
            reply(422, &[], fixture("ledger_put_no_sha.json")),
        );
    let waits = Waits::default();

    let outcome = ledger(&recorded, &waits).write(&october(), b"foo", &WriteCondition::Absent);

    assert_unconfirmed(&outcome);
    assert_eq!(recorded.writes().len(), 2);
}

#[test]
fn concurrent_appends_through_the_adapter_lose_no_record() {
    let rival = fixture("ledger_line.jsonl");
    let api = ContentsApi::default().rival_writes(october().as_str(), &rival);
    let waits = Waits::default();
    let adapter = ledger(&api, &waits);

    let appended = append_cost(&adapter, &reservation(1));

    assert_eq!(appended.ok(), Some(Appended::Recorded));
    let window = read_costs(&adapter, at("2026-10-12T10:00:00.000Z")).expect("the file reads");
    let runs: Vec<u64> = window
        .records()
        .iter()
        .map(|record| record.run_uid.run_id.get())
        .collect();
    assert_eq!(runs, [7001, 1]);
    assert_eq!(window.day_spent(&[]), Aeu::from_micros(1_480_000));
    assert_eq!(api.puts(), 2, "one conflict, one write");
}
