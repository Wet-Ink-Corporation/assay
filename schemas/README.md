# schemas/

JSON Schemas (Draft 2020-12) for every versioned contract (`docs/architecture/02-architecture.md` §7, `03-contracts.md`):

| Contract | Schema | Spec |
|---|---|---|
| `assay.findings/1` | `findings-1.schema.json` | 03 §3 |
| `assay.run/1` | `run-1.schema.json` | 03 §4 |
| `assay.decision/1` | `decision-1.schema.json` | 03 §5 |
| `assay.event/1` | `event-1.schema.json` | 03 §6 |
| `assay.run-inputs/1` | `run-inputs-1.schema.json` | 03 §6a |
| `assay.status/1` | `status-1.schema.json` | 03 §6b |

Every document carries its contract and version in a `schema` field (`"assay.run/1"`). Unknown fields are rejected (`additionalProperties: false`) with two deliberate exceptions, both maps whose keys the spec leaves open:

- the keys of `artifact_names` in `assay.run/1` (one per-job nonce name per job; each value must still be a non-empty string);
- `data` in `assay.event/1` for the kinds whose fields 03 §6 does not list (`RunRequested`, `PlanComputed`, `CoordinatorFinished`, `FindingRaised`, `DecisionMade`, `CheckPublished`, `RunSuperseded`, `BudgetDeferred`, `SlotBusy`, `LedgerWritten`, `SlotReleased`); it must still be an object. The `assay-domain` types are stricter and refuse any field there, so a field added to one of these kinds needs every reader upgraded first (#147).

Text caps (`maxLength`, counted in characters) reject over-length input rather than truncating it.

`assay-app` embeds these files and validates every document on read (`ContractValidator::read`), refusing outright a document over 8 MiB (`MAX_DOCUMENT_BYTES`) or one in which an object repeats a key; a rejection is disposition `schema`, and its error never quotes the document (keys the schema does not name are reported as `*`). A version is never edited once released: a new version is a new file plus an upcast, and is a human decision (H-16).

`fixtures/<contract>/valid/` and `fixtures/<contract>/invalid/` are the golden documents. `crates/assay-app/tests/it/contract.rs` accepts every valid one, and rejects each invalid one at the JSON Pointer and keyword it lists; an invalid fixture without an entry there fails the test. `fixtures/event/domain-invalid/` holds the reverse: documents the schema accepts and the `assay-domain` types refuse (`run-requested-with-data.json`, data on a kind whose fields 03 §6 does not list, #147). `contract.rs` accepts each, and `crates/assay-domain/tests/it/round_trip.rs` refuses each with the message it lists, as it does every invalid event and decision fixture. `assay.run-inputs/1` is not a `Contract`: `assay-app` writes it and reads it back (replay, `assay status`, and the next run's carry set, #224) through its own strict serde DTO, since a bundle may pass the 8 MiB contract-read cap; `crates/assay-app/tests/it/publish_persist.rs` checks its fixtures the same way and validates every bundle `publish` writes against it.

The `assay.status/1` goldens in `fixtures/status/valid/` (success, failure with blockers, in progress, pending budget-deferred, refused (#218), no check, forged check ignored) are the exact documents `crates/assay-app/tests/it/status.rs` expects `assay status --json` to produce, and are the fixtures for consumers such as the mod's tests. `local-fresh.json`, `local-stale.json` and `local-missing.json` are the same for `assay status --json --local` (`source: "local"`, #130), which `crates/assay-app/tests/it/status_local.rs` expects. A recorded `assay.event/1` stream of `assay check --local --events`, every line valid and in `(job, idx)` order, is `crates/assay-app/tests/fixtures/events/check-local.jsonl`; `crates/assay-app/tests/it/check_events.rs` produces it byte for byte.
