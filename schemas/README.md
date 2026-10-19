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
| `assay.ledger-cost/1` | `ledger-cost-1.schema.json` | 03 §6c |
| `assay.ledger-push/1` | `ledger-push-1.schema.json` | 03 §6c |
| `assay.ledger-command/1` | `ledger-command-1.schema.json` | 03 §6c |
| `assay.ledger-state/1` | `ledger-state-1.schema.json` | 03 §6c |

Every document carries its contract and version in a `schema` field (`"assay.run/1"`). Unknown fields are rejected (`additionalProperties: false`) with two deliberate exceptions, both maps whose keys the spec leaves open:

- the keys of `artifact_names` in `assay.run/1` (one per-job nonce name per job; each value must still be a non-empty string);
- `data` in `assay.event/1` for the kinds whose fields 03 §6 does not list (`RunRequested`, `PlanComputed`, `CoordinatorFinished`, `FindingRaised`, `DecisionMade`, `CheckPublished`, `RunSuperseded`, `BudgetDeferred`, `SlotBusy`, `LedgerWritten`, `SlotReleased`); it must still be an object.

Text caps (`maxLength`, counted in characters) reject over-length input rather than truncating it.

`assay-app` embeds these files and validates every document on read (`ContractValidator::read`), refusing outright a document over 8 MiB (`MAX_DOCUMENT_BYTES`) or one in which an object repeats a key; a rejection is disposition `schema`, and its error never quotes the document (keys the schema does not name are reported as `*`). A version is never edited once released: a new version is a new file plus an upcast, and is a human decision (H-16).

`fixtures/<contract>/valid/` and `fixtures/<contract>/invalid/` are the golden documents. `crates/assay-app/tests/it/contract.rs` accepts every valid one, and rejects each invalid one at the JSON Pointer and keyword it lists; an invalid fixture without an entry there fails the test. `assay.run-inputs/1` is not a `Contract`: `assay-app` writes it and reads it back (replay, `assay status`, and the next run's carry set, #224) through its own strict serde DTO, since a bundle may pass the 8 MiB contract-read cap; `crates/assay-app/tests/it/publish_persist.rs` checks its fixtures the same way and validates every bundle `publish` writes against it. The four ledger record contracts (#70) are not `Contract`s either: `assay-app`'s ledger operations write them and read them back through strict serde DTOs (one per JSONL line, or the state document); `crates/assay-app/tests/it/ledger_schema.rs` checks that schema and reader accept every valid golden under `fixtures/ledger-*/valid/` and refuse every invalid one, each at the pointer and keyword it lists, and that every record the writer produces passes its schema. Uniqueness rules (one record per key, `seq` in sequence, a cost record's `at` in its file's month) span lines and are enforced by the reader alone.

The `assay.status/1` goldens in `fixtures/status/valid/` (success, failure with blockers, in progress, pending budget-deferred, refused (#218), no check, forged check ignored) are the exact documents `crates/assay-app/tests/it/status.rs` expects `assay status --json` to produce, and are the fixtures for consumers such as the mod's tests. `local-fresh.json`, `local-stale.json` and `local-missing.json` are the same for `assay status --json --local` (`source: "local"`, #130), which `crates/assay-app/tests/it/status_local.rs` expects. A recorded `assay.event/1` stream of `assay check --local --events`, every line valid and in `(job, idx)` order, is `crates/assay-app/tests/fixtures/events/check-local.jsonl`; `crates/assay-app/tests/it/check_events.rs` produces it byte for byte.
