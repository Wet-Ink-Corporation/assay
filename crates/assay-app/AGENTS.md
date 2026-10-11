# Crate charter: `assay-app`

## Purpose
The use cases (`open`, `plan`, `admit`, `gate`, `review`, `decide`, `replay`, `report`, `doctor`) and the port traits they need (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`). It is its own crate because the ports must be owned by their consumer so adapters depend inward (ARC-03, `docs/architecture/02-architecture.md` §2).

## Layer
application. Orchestrates domain decisions and effects through ports. No concrete adapter, no HTTP or process types, no runtime choice, no `anyhow` in `pub` signatures.

## Allowed dependencies
- Workspace crates: `assay-domain` only, added by the story that first uses it.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added.
- Never: `assay-forge`, `assay-runner`, `assay-gates`, `assay-cli`, `weighin-core` or temper code (P8). `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
Workspace-internal, not published (`publish = false`). Re-exported from `lib.rs`:
- `Contract`, `ContractValidator` (`new`, `read`), `SchemaViolation`: validate-on-read for the versioned contracts, against the schemas in `schemas/` embedded with `include_str!` (`src/contract.rs`).

## Invariants
- Ports speak domain types; no adapter type crosses a port signature (ARC-03).
- A contract document is accepted only at a supported version and only if it satisfies that version's schema; unknown fields, unknown versions and over-length text are rejected, never repaired (03 preamble).
- Validation errors never quote the rejected value (it is untrusted text).

## Errors
- `ReadContractError` (`Malformed`, `MissingSchema`, `UnknownSchema`, `UnknownVersion`, `Invalid`): every variant maps to disposition `schema` through `disposition()`.
- `ContractSchemaError`: an embedded schema does not compile, a defect in the build.
- Use cases to come: one `thiserror` enum each; port failures surface through each port's associated error type.

## Test strategy
Integration tests in `tests/it/` drive use cases against in-memory fakes of the ports (TST-09); no real network, processes or clocks. Run: `cargo test -p assay-app`.

## Do not
- Do not call `gh`, `claude` or the file system directly; add or extend a port.
- Do not read environment variables; configuration arrives parsed from the CLI (ARC-11).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); the edge to `assay-domain` is added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | Contract validation lives here, not in the domain (whose dependencies are fixed to serde, thiserror and sha2) and not in an adapter (it does no I/O and every adapter needs it). Adds `assay-domain` (for `Disposition`), `jsonschema` (no default features: no HTTP or file resolution), `serde_json` and `thiserror`, all pre-approved. | ARC-02, ARC-03 | Story #47 |
| 2026-10-11 | `ContractValidator` is a value built once by the composition root, not a lazily initialised static: no hidden global state, and the fallible compile step surfaces as a typed error. | OWN-11, ERR-01 | Story #47 |
