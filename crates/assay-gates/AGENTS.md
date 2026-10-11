# Crate charter: `assay-gates`

## Purpose
The gate adapters: the built-in `temper` and `weigh-in` adapters and the generic `exec` adapter, configured by `.assay/plugins.toml`. It implements the `GateRunner` port of `assay-app`. temper and Weigh-In knowledge lives only here, as parsers of their documented JSON (P8). It is its own crate because it is the one place that knows sibling tools' contracts (ARC-07, `docs/architecture/02-architecture.md` §2).

## Layer
adapter. Implements application ports; each tool's JSON shape is a DTO here, converted with `TryFrom` into domain types (OUT-01). No business rules.

## Allowed dependencies
- Workspace crates: `assay-app` and `assay-domain`, added by the story that first uses them.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added.
- Never: `weighin-core` or any temper code: assay consumes them through contracts only (P8). Never `assay-forge`, `assay-runner` or `assay-cli`. `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
None yet. Workspace-internal, not published (`publish = false`).

## Invariants
- Gate tools run as external processes with timeouts and size-capped output (H-13).

## Errors
None yet. A `thiserror` enum per adapter, separating a failed gate (a result) from a gate that could not run (ERR-10).

## Test strategy
Integration tests in `tests/it/` parse recorded tool outputs; missing contract surface is requested upstream, not reverse-engineered. Run: `cargo test -p assay-gates`.

## Do not
- Do not link or vendor temper or Weigh-In code to read their state.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
