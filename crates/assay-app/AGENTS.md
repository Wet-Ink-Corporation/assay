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
None yet. Workspace-internal, not published (`publish = false`).

## Invariants
- Ports speak domain types; no adapter type crosses a port signature (ARC-03).

## Errors
None yet. One `thiserror` enum per use case; port failures surface through each port's associated error type.

## Test strategy
Integration tests in `tests/it/` drive use cases against in-memory fakes of the ports (TST-09); no real network, processes or clocks. Run: `cargo test -p assay-app`.

## Do not
- Do not call `gh`, `claude` or the file system directly; add or extend a port.
- Do not read environment variables; configuration arrives parsed from the CLI (ARC-11).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); the edge to `assay-domain` is added when first used. | ARC-12, CRF-05 | Story #46 |
