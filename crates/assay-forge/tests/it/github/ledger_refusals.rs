//! Each shape check of a ledger read refuses the file as `Malformed`, naming the check, and asks
//! GitHub for nothing more (wi-b5affa).

use assay_app::Ledger;
use assay_forge::github::{ForgeError, LedgerRepoError, Method, Operation};
use serde_json::json;

use super::contents::{LEDGER_REPOSITORY, contents_path, ledger, october};
use super::recorded::{Recorded, Waits, fixture, fixture_json, json, reply};

/// The blob `ledger_contents_none.json` lists.
const BLOB_SHA: &str = "8f9e2b0c41f6a2a7d1f0c9e3b5a4d6c7e8f90123";

/// Reading `october()` through `recorded` is refused as `Malformed` with `detail`, after exactly
/// `requests` requests and none left unasked.
fn refused(recorded: &Recorded, requests: usize, detail: &str) {
    let waits = Waits::default();

    let file = ledger(recorded, &waits).read(&october());

    assert!(
        matches!(
            &file,
            Err(LedgerRepoError::Forge(ForgeError::Malformed {
                operation: Operation::ReadLedgerFile,
                detail: refused,
            })) if *refused == detail
        ),
        "{file:?}"
    );
    assert_eq!(recorded.unused(), 0, "every scripted answer was asked for");
    assert_eq!(recorded.sent().len(), requests, "nothing further was asked");
}

/// `october()`'s contents answered with `body`.
fn listed(body: &serde_json::Value) -> Recorded {
    Recorded::default().ok(Method::Get, &contents_path(&october()), json(body))
}

#[test]
fn an_unknown_content_encoding_is_refused() {
    let mut body = fixture_json("ledger_contents.json");
    body["encoding"] = json!("utf-8");

    refused(&listed(&body), 1, "unknown content encoding");
}

#[test]
fn a_repository_check_answered_for_another_repository_is_refused() {
    let mut other = fixture_json("ledger_repository.json");
    other["full_name"] = json!("Wet-Ink-Corporation/assay-ledger-copy");
    let recorded = Recorded::default()
        .then(
            Method::Get,
            &contents_path(&october()),
            reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
        )
        .ok(
            Method::Get,
            &format!("repos/{LEDGER_REPOSITORY}"),
            json(&other),
        );

    refused(&recorded, 2, "the API answered for another repository");
}

#[test]
fn a_blob_other_than_the_one_listed_is_refused() {
    let changes = [
        ("sha", json!("1acc419d4d6a9ce985db7be48c6349a0475975b5")),
        ("size", json!(394)),
        ("encoding", json!("utf-8")),
    ];
    for (field, value) in changes {
        let mut blob = fixture_json("ledger_blob.json");
        blob[field] = value;
        let recorded = Recorded::default()
            .ok(
                Method::Get,
                &contents_path(&october()),
                fixture("ledger_contents_none.json"),
            )
            .ok(
                Method::Get,
                &format!("repos/{LEDGER_REPOSITORY}/git/blobs/{BLOB_SHA}"),
                json(&blob),
            );

        refused(
            &recorded,
            2,
            "the blob is not the one the contents API listed",
        );
    }
}

#[test]
fn a_version_that_is_not_a_full_lowercase_blob_id_is_refused() {
    let shas = [
        "3d21ec53a331a6f037a91c368710b99387d012c",
        "3D21EC53A331A6F037A91C368710B99387D012C1",
        "3d21ec53a331a6f037a91c368710b99387d012cg",
    ];
    for sha in shas {
        let mut body = fixture_json("ledger_contents.json");
        body["sha"] = json!(sha);

        refused(&listed(&body), 1, "the file's sha is not a full blob id");
    }
}
