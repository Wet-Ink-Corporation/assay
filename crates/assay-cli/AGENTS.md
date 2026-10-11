# Crate charter: `assay-cli`

## Purpose
The `assay` binary: the composition root. It parses arguments, loads and validates configuration once, wires the adapters into the use cases and maps outcomes to exit codes (`docs/architecture/02-architecture.md` §2). It also hosts the workspace's architecture tests, because it is the one crate that may depend on every other.

## Layer
binary. Thin (ARC-14): no business logic, nothing a test would want to call directly.

## Allowed dependencies
- Workspace crates: all five others, added by the story that first wires each.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added. Dev only: `serde_json` (parses `cargo metadata` in the architecture test).
- Never: `weighin-core` or temper code (P8).

## Public surface
The `assay` command line. No library target.

## Invariants
- `main` returns `ExitCode`; nothing calls `process::exit` (ARC-14).
- Output goes through locked handles with `writeln!`, never print macros (H-06).

## Errors
None yet. `anyhow` with `.context()` is allowed here only (ERR-06), once approved as a dependency.

## Test strategy
One integration binary, `tests/it/` (TST-14):
- `architecture.rs` with `layering.rs`: the ADR-07 crate graph read from `cargo metadata`, plus synthetic graphs proving each rule rejects a violation.
- `layout.rs`: the repository layout of 02 §7.
- `binary.rs`: the `assay` binary runs.
- `fetch_by_digest.rs`: `.github/actions/fetch-by-digest/fetch-by-digest.sh` against a fake `oras` (no network): a digest, artifact-type, media-type or size mismatch aborts with nothing installed, malformed repositories and out-dirs are usage errors, and the token travels on stdin (ADR-14, 05 T14). Unix only; needs `bash`, `jq`, `sha256sum` and `python3` >= 3.11.
- `release_workflow.rs`: `.github/workflows/release.yml` runs only on `v*` tags with least-privilege jobs, and every action in `.github/` is pinned by commit SHA.

Run: `cargo test -p assay-cli`.

## Do not
- Do not put use-case logic here; it belongs in `assay-app`.
- Do not relax `tests/it/layering.rs` to admit a new crate or edge without the H-16 decision that allows it.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | The architecture test lives here rather than in a seventh crate or an xtask, keeping ADR-07's six crates. | ARC-07, ARC-02 | Story #46 |
| 2026-10-11 | `serde_json` (pre-approved) as a dev-dependency for the architecture test. | ARC-12 | Story #46 |
