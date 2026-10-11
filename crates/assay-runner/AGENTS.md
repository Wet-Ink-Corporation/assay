# Crate charter: `assay-runner`

## Purpose
The model-runner adapter: builds and supervises `claude -p` processes, parses `--output-format stream-json --verbose`, accounts for cost from `result` messages, and runs the auth and session-init preflight. It implements the `ModelRunner` port of `assay-app`. It is its own crate because it holds the headless profile and is the only code that starts a model process (ARC-07, `docs/architecture/02-architecture.md` §2, §7).

## Layer
adapter. Implements application ports; stream-json DTOs live here and are converted with `TryFrom` into domain types (OUT-01). No business rules.

## Allowed dependencies
- Workspace crates: `assay-app` and `assay-domain`, added by the story that first uses them.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added.
- Never: `assay-forge`, `assay-gates`, `assay-cli`, `weighin-core` or temper code (P8). `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
None yet. Workspace-internal, not published (`publish = false`).

## Invariants
- Every spawned process has an owner that waits on or kills it, and a timeout (H-13).
- Output read from a process is size-capped (H-13).

## Errors
None yet. A `thiserror` enum per operation, separating model refusals from process failures (ERR-10).

## Test strategy
Integration tests in `tests/it/` parse recorded stream-json transcripts; no live model calls in tests (TST-10). Run: `cargo test -p assay-runner`.

## Do not
- Do not trust model output as authoritative; it is validated mechanically downstream (ADR-08).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
