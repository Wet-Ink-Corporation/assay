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
Workspace-internal, not published (`publish = false`). Re-exported from `lib.rs`:
- `ExecAdapter<S>` (`new`; implements `assay_app::GateRunner`), `ExecSettings`, `ExecLimits` (`DEFAULT`), `FINDINGS_OUT_VAR`, `FINDINGS_FILE`: the `exec` adapter (`src/exec.rs`).
- `Supervisor` (implemented for `&mut S` too) and `ProcessSpec`: the bounded process runner the exec adapter is generic over. No production implementation lives here yet; see Decisions.
- `GateEnvironment` (`from_vars`, `vars`): the inherited-variable allowlist (`src/environment.rs`).

## Invariants
- Gate tools run as external processes with timeouts and size-capped output (H-13).
- A gate process sees only `PATH`, `HOME`, `CI` and `ASSAY_*` (exact, case-sensitive names) plus the adapter's `ASSAY_FINDINGS_OUT`, which always overrides an inherited one.
- `exec` runs only in its own Z2b job: `gates-static`, another Z2b job or a non-`exec` plugin is refused before anything runs.
- Each run writes into `<output_root>/<plugin>/`, created fresh; an existing directory is refused, so a stale document is never read. The document is read only when it is a regular file (never through a symlink) of at most `max_document_bytes`.

## Errors
`ExecError<E>` (`NotExec`, `OutsideBuildJob`, `WrongJob`, `OutputDirectory`, `Supervisor(E)`, `ReadDocument`): the gate could not run, recorded by the use case as `gate-error:not-run`. A gate that ran and failed is a `GateRun`, never an error (ERR-10).

## Test strategy
Integration tests in `tests/it/` parse recorded tool outputs; missing contract surface is requested upstream, not reverse-engineered. The exec adapter is tested against a scripted `Supervisor` that plays the gate process (`tests/it/support.rs`). Run: `cargo test -p assay-gates`.

## Do not
- Do not link or vendor temper or Weigh-In code to read their state.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | Adds the `assay-app` edge and `thiserror` with the `exec` adapter. The process-group supervisor in `assay-forge` is not reachable from here (forbidden edge, and `pub(crate)`), so the adapter is generic over `Supervisor`; where the real supervisor lives is an open crate-boundary decision. | ARC-02, H-13, H-16 | Story #62, owner to decide |
