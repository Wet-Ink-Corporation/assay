# Crate charter: `assay-forge`

## Purpose
The GitHub adapter: REST and GraphQL through `gh`, App-token minting narrowed per job, diffs and objects, checks (compare-and-swap by check id and `external_id`), comments, timeline events, reactions and the ledger repository. It implements the `Forge` and `Ledger` ports of `assay-app`. It is its own crate because it is the only code that talks to GitHub and decides who counts as an owner (ARC-07, `docs/architecture/02-architecture.md` §2, §7).

## Layer
adapter. Implements application ports; wire DTOs for GitHub live here and are converted with `TryFrom` into domain types (OUT-01). No business rules.

## Allowed dependencies
- Workspace crates: `assay-app` and `assay-domain`, added by the story that first uses them.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added.
- Never: `assay-runner`, `assay-gates`, `assay-cli`, `weighin-core` or temper code (P8). `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
None yet. Workspace-internal, not published (`publish = false`).

## Invariants
- Every external call has a timeout and a bounded response size (H-13).

## Errors
None yet. A `thiserror` enum per operation, separating GitHub rejections from transport failures (ERR-10).

## Test strategy
Integration tests in `tests/it/` against recorded `gh` responses; no live network in tests (TST-10). Run: `cargo test -p assay-forge`.

## Do not
- Do not decide verdicts or severities here; that is the domain's job.
- Do not hold a write token longer than the operation needs.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
