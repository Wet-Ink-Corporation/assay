# schemas/

JSON Schemas (Draft 2020-12) for every versioned contract (`docs/architecture/02-architecture.md` §7, `03-contracts.md`):

| Contract | Schema | Spec |
|---|---|---|
| `assay.findings/1` | `findings-1.schema.json` | 03 §3 |
| `assay.run/1` | `run-1.schema.json` | 03 §4 |
| `assay.decision/1` | `decision-1.schema.json` | 03 §5 |
| `assay.event/1` | `event-1.schema.json` | 03 §6 |

Every document carries its contract and version in a `schema` field (`"assay.run/1"`). Unknown fields are rejected (`additionalProperties: false`) with two deliberate exceptions, both maps whose keys the spec leaves open:

- the keys of `artifact_names` in `assay.run/1` (one per-job nonce name per job; each value must still be a non-empty string);
- `data` in `assay.event/1` for the kinds whose fields 03 §6 does not list (`RunRequested`, `PlanComputed`, `CoordinatorFinished`, `FindingRaised`, `DecisionMade`, `CheckPublished`, `RunSuperseded`, `BudgetDeferred`, `SlotBusy`, `LedgerWritten`, `SlotReleased`); it must still be an object.

Text caps (`maxLength`, counted in characters) reject over-length input rather than truncating it.

`assay-app` embeds these files and validates every document on read (`ContractValidator::read`), refusing outright a document over 8 MiB (`MAX_DOCUMENT_BYTES`) or one in which an object repeats a key; a rejection is disposition `schema`, and its error never quotes the document (keys the schema does not name are reported as `*`). A version is never edited once released: a new version is a new file plus an upcast, and is a human decision (H-16).

`fixtures/<contract>/valid/` and `fixtures/<contract>/invalid/` are the golden documents. `crates/assay-app/tests/it/contract.rs` accepts every valid one, and rejects each invalid one at the JSON Pointer and keyword it lists; an invalid fixture without an entry there fails the test.
