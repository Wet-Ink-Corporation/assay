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
- `ExecAdapter<S>` (`new`; implements `assay_app::GateRunner`), `ExecSettings` (`new`, refusing a relative checkout or output root with `ExecSettingsError`), `ExecLimits` (`DEFAULT`), `FINDINGS_OUT_VAR`, `FINDINGS_FILE`: the `exec` adapter (`src/exec.rs`).
- `Supervisor` (implemented for `&mut S` too) and `ProcessSpec`: the bounded process runner the exec adapter is generic over. The production implementation is `assay_cli::ForgeSupervisor`, which delegates to `assay-forge`'s process-group supervisor (wi-603c63); this crate does not depend on forge.
- `GateEnvironment` (`from_vars`, `vars`): the inherited-variable allowlist (`src/environment.rs`).
- `CaptureSupervisor` (implemented for `&mut S` too) and `CapturedRun`: a bounded run whose stdout is kept (`src/capture.rs`, #63's, shared).
- `PinnedCargo<S: CaptureSupervisor>` (`new`, `metadata(copy)`), `PinnedToolchain`, `CargoSettings` (`new`, refusing relative binaries or `CARGO_HOME` and an empty toolchain name with `CargoSettingsError`), `CargoEnvironment` (`from_vars`, `vars`), `CargoLimits` (`DEFAULT`), `CargoStep`, `CargoError`: the pinned base cargo on a sanitised manifest-only copy, for exec-surface detection in `gates-static` (`src/cargo.rs`, 04 §2.2, #65).

## Invariants
- Gate tools run as external processes with timeouts and size-capped output (H-13).
- A gate process sees only `PATH`, `HOME`, `CI`, `ASSAY_BASE_SHA`, `ASSAY_HEAD_SHA` and `ASSAY_RUN_KEY` (exact, case-sensitive names, 03 §2; no other `ASSAY_*` variable) plus the adapter's `ASSAY_FINDINGS_OUT`; an inherited `ASSAY_FINDINGS_OUT` never passes the allowlist.
- The checkout and the output root are absolute paths (`ExecSettings::new`), so neither depends on this process's working directory.
- `exec` runs only in its own Z2b job: `gates-static`, another Z2b job or a non-`exec` plugin is refused before anything runs.
- The pinned cargo runs `cargo fetch --locked`, then (only if that exits 0) `cargo metadata --locked --offline --format-version 1 --all-features`, both with `--manifest-path <copy>/Cargo.toml`, `--config net.git-fetch-with-cli=false` and the copy as working directory. It sees only `PATH`, `HOME`, `RUSTUP_HOME` and the proxy variables, plus `CARGO_HOME`, `RUSTUP_TOOLCHAIN`, `RUSTC` (all from `CargoSettings`) and `CARGO_TERM_COLOR=never`; every inherited `CARGO_*`, `RUSTC*`, `RUSTFLAGS` or `RUSTUP_TOOLCHAIN` is dropped. Anything but exit 0 is `CargoError`, never a partial result. Precondition, checked before anything runs: no directory above the copy holds a `Cargo.toml` or a `.cargo` entry (cargo reads both from every ancestor of its working directory), else `CargoError::UnsafeAncestor`, or `InspectAncestor` when one cannot be inspected; the composition root places copies in a scratch directory of their own.
- Each run writes into `<output_root>/<plugin>/`, created fresh; an existing directory is refused, so a stale document is never read. The document is read only when it is a regular file (never through a symlink) of at most `max_document_bytes`.

## Errors
`ExecError<E>` (`NotExec`, `OutsideBuildJob`, `WrongJob`, `OutputDirectory`, `Supervisor(E)`, `ReadDocument`): the gate could not run, recorded by the use case as `gate-error:not-run`. A gate that ran and failed is a `GateRun`, never an error (ERR-10). `ExecSettingsError` (`RelativeCheckout`, `RelativeOutputRoot`): the composition root passed a relative directory. `CargoError<E>` (`CopyPath`, `UnsafeAncestor`, `InspectAncestor`, `Supervisor(E)`, `Failed` with the `CargoStep` and `Termination`, displayed in words, `NotUtf8`): no package set for that side, which `assay_app::exec_surface` reads as unavailable (fail closed). `CargoSettingsError` (`RelativeCargo`, `RelativeRustc`, `EmptyToolchain`, `RelativeCargoHome`).

## Known gaps
- Escaping the group kill: a gate is PR-controlled code, and a process it starts can leave the gate's process group with `setsid` or `setpgid`. The supervisor kills and waits for the group only, so such a process is neither killed at the deadline nor waited for, and can outlive the gate run (and write into the output directory afterwards). Only the job's VM or container boundary contains it; closing it needs a cgroup or PID-namespace supervisor (H-16, #150 review F5).
- FIFO swap (`read_document`): the document's type check and its open are two calls. The supervisor has shown the gate's group gone first, so only a process that left the group (`setsid`) could swap a FIFO in between, which would block the open. Closing it needs `O_NOFOLLOW | O_NONBLOCK`, which std cannot name without `libc` (H-16; #152 F8).

## Test strategy
Integration tests in `tests/it/` parse recorded tool outputs; missing contract surface is requested upstream, not reverse-engineered. The exec adapter is tested against a scripted `Supervisor` that plays the gate process (`tests/it/support.rs`), and the pinned cargo against a scripted `CaptureSupervisor` (`tests/it/cargo.rs`: argv, working directory, exact environment, step order and failures); the real cargo runs in `assay-cli`'s `tests/it/exec_surface.rs`. Run: `cargo test -p assay-gates`.

## Do not
- Do not link or vendor temper or Weigh-In code to read their state.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | Adds the `assay-app` edge and `thiserror` with the `exec` adapter. The process-group supervisor in `assay-forge` is not reachable from here (forbidden edge, and `pub(crate)`), so the adapter is generic over `Supervisor`; where the real supervisor lives is an open crate-boundary decision. | ARC-02, H-13, H-16 | Story #62, owner to decide |
| 2026-10-12 | Decided: forge's supervisor is made public and `assay-cli` implements `Supervisor` with it (`ForgeSupervisor`); no new edge from this crate. | ADR-07, H-16 | owner, Weigh-In wi-603c63 (decided A), #150 |
| 2026-10-12 | #65: the pinned base cargo for exec-surface detection lives here, as a Z2s pinned-tool adapter beside the temper adapter (`gates-static`, 02 §3), not in forge (whose single-constructor test admits only git and the kill program). It takes #63's `CaptureSupervisor` verbatim (claude/assay-63-temper-adapter at b26ed86), which keeps stdout only, so a failed run reports its termination without cargo's stderr. `RUSTC` is set explicitly because `cargo metadata` runs `rustc -vV` and a cleared `PATH` lookup would otherwise decide which rustc. | ADR-07, ADR-12, H-13 | #65 |
| 2026-10-12 | #152 review follow-ups: only the three `ASSAY_*` variables 03 §2 names pass (F7), `ExecSettings` is built by a validating `new` with private fields (F2, an API change inside the workspace), `WrongJob` displays the job as `gates-lint`/`gates-test` (F10), and the FIFO gap is recorded above (F8). | H-13, API-03, H-16 | #152, #150 |
