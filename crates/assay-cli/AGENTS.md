# Crate charter: `assay-cli`

## Purpose
The `assay` binary: the composition root. It parses arguments, loads and validates configuration once, wires the adapters into the use cases and maps outcomes to exit codes (`docs/architecture/02-architecture.md` §2). It also hosts the workspace's architecture tests, because it is the one crate that may depend on every other.

## Layer
binary. Thin (ARC-14): no business logic, nothing a test would want to call directly.

## Allowed dependencies
- Workspace crates: all five others, added by the story that first wires each.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added. Dev only: `serde_json` (parses `cargo metadata` in the architecture test), `tempfile` (scratch directories in the supervisor tests).
- Wired so far: `assay-app`, `assay-forge`, `assay-gates`, `assay-runner` (#150); `assay-domain` as a dev-dependency.
- Never: `weighin-core` or temper code (P8).

## Public surface
The `assay` command line. A library target, `assay_cli`, holds the adapters the composition root lends one crate from another, so they are tested before a command wires them: `ForgeSupervisor` (`new`), `assay-forge`'s process-group supervisor as an `assay_gates::Supervisor`, an `assay_gates::CaptureSupervisor` (stdout kept; none after a timeout or an over-cap run) and an `assay_runner::GroupKill` (wi-603c63). Nothing else is public. As a `Supervisor` it discards the gate's output, but forge reads it back first: a gate run briefly holds up to twice `max_output_bytes` in memory (32 MiB at `ExecLimits::DEFAULT`), accepted because one gate runs at a time.

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
- `exec_surface.rs`: exec-surface detection end to end (04 §2.2, #65) on fixture PRs built as git repositories and fetched into an `ObjectRepo`: forge's manifest copy, the real cargo that built the tests as the pinned cargo (`PinnedCargo` under `ForgeSupervisor`) with registry crates from a directory source in the test's own `CARGO_HOME` (no network), and `assay_app::exec_surface`. A new `build.rs` and a bumped crate gaining a build script are exec surface, a plain registry bump is only `Registry`, a path build-dependency inherited through `[workspace.dependencies]` is an exec root, an unresolvable `workspace = true` fails closed, and a PR's `.cargo/config.toml` wrapper, `rust-toolchain.toml` and `.gitattributes` never reach cargo (the wrapper, outside the tree, would leave a marker). Unix only; needs git.
- `fetch_by_digest.rs`: `.github/actions/fetch-by-digest/fetch-by-digest.sh` against a fake `oras` (no network): a digest, artifact-type, media-type or size mismatch aborts with nothing installed, malformed repositories and out-dirs are usage errors, and the token travels on stdin (ADR-14, 05 T14). Unix only; needs `bash`, `jq`, `sha256sum` and `python3` >= 3.11.
- `release_workflow.rs`: `.github/workflows/release.yml` runs only on `v*` tags with least-privilege jobs, and every action in `.github/` is pinned by commit SHA.
- `supervisor.rs`: `ForgeSupervisor` with the system `kill`: a gate's exit code, signal, timeout (its whole group gone), output cap, working directory and exact environment; and `ClaudeHeadless` with it against a stand-in `claude` that leaves a helper in its group, which must be dead after a timeout and after a clean exit. Linux only (`/proc`, procps or util-linux `kill`).

Run: `cargo test -p assay-cli`.

## Do not
- Do not put use-case logic here; it belongs in `assay-app`.
- Do not relax `tests/it/layering.rs` to admit a new crate or edge without the H-16 decision that allows it.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | The architecture test lives here rather than in a seventh crate or an xtask, keeping ADR-07's six crates. | ARC-07, ARC-02 | Story #46 |
| 2026-10-11 | `serde_json` (pre-approved) as a dev-dependency for the architecture test. | ARC-12 | Story #46 |
| 2026-10-12 | `ForgeSupervisor` implements `CaptureSupervisor` (#63's implementation, taken verbatim by #65 for the pinned cargo); no new edge. Exec-surface detection is composed only in `tests/it/exec_surface.rs` until the `plan`/`gates-static` command wires it. | ADR-07 | #65 |
| 2026-10-12 | `ForgeSupervisor` lends forge's supervisor to the gate and model-runner adapters (wi-603c63). It lives in a library target because no command wires it yet (the `assay gate` command is the cli story's) and a binary-only module `main` never calls is dead code; the target exposes only these adapters. Adds the `assay-app`, `assay-forge`, `assay-gates` and `assay-runner` edges, and `assay-domain` and `tempfile` (pre-approved) as dev-dependencies. | ADR-07, ARC-14, CRF-05 | #150; library target to confirm by the owner |
