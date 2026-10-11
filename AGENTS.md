# assay

assay is the owner's PR review pipeline: it gates pull requests on mechanical checks (temper, Weigh-In and `exec` gates) and on model review run headlessly with `claude -p`, and publishes a single verdict through a dedicated GitHub App. It serves the owner's own private Rust repositories, assay itself included. The specification is `docs/architecture/`; start at its `README.md`.

## Crate map

| Crate | Layer | Purpose | Depends on |
|---|---|---|---|
| `crates/assay-domain` | domain | findings, severities, fingerprints, rubric, tiers, run state machine, events; no I/O | std; only `serde`, `thiserror`, `sha2` |
| `crates/assay-app` | application | use cases and the port traits (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`) | domain |
| `crates/assay-forge` | adapter | GitHub through `gh`: tokens, diffs, checks, comments, timeline, ledger repo | app, domain |
| `crates/assay-runner` | adapter | supervises `claude -p`, parses stream-json, accounts for cost | app, domain |
| `crates/assay-gates` | adapter | `temper`, `weigh-in` and `exec` gate adapters | app, domain |
| `crates/assay-cli` | binary (`assay`) | composition root: config, wiring, exit codes | everything |

Dependencies point inward: domain, then application, then adapters, then the binary (ARC-02, ADR-07). No crate links `weighin-core` or any temper code (P8). `crates/assay-cli/tests/it/architecture.rs` reads `cargo metadata` and fails on any other edge, on a domain dependency outside the three above, on a seventh crate and on a publishable crate. Each crate's `AGENTS.md` is its charter (ARC-08).

## Repository layout

`docs/architecture/02-architecture.md` §7. `schemas/` holds the contract JSON Schemas and their golden fixtures (its README indexes them). Directories without content yet (`plugin/`, `workflows/`, `rubric/`, `corpus/`, `docs/decisions/`) carry a README; `crates/assay-cli/tests/it/layout.rs` checks them.

## Commands

| Purpose | Command |
|---|---|
| Format | `cargo fmt --all` |
| Lint | `node .claude/temper/temper.mjs baseline --check`: clippy, rustdoc and temper's scan, as strict as `-D warnings` (there is no baseline) |
| Test | `cargo nextest run --workspace` (or `cargo test --workspace`), plus `cargo test --doc --workspace` |
| Docs | `cargo doc --workspace --no-deps` (rustdoc lints are part of Lint) |
| Supply chain | `cargo deny check` |
| CI-only checks | `cargo check -p <crate>` for each crate (ARC-16), `cargo hack check --workspace --feature-powerset` (API-16), `cargo machete` (CRF-05) |
| Full gate (Claude Code) | `/temper:gate`, or `node "$TEMPER_CLI" gate` |

Toolchain 1.98.1 (`rust-toolchain.toml`). MSRV 1.96 (`rust-version`); policy: the template's stable-minus-two, raised only as an H-16 decision (MOD-01). The release profile is the template's; changes are recorded below (PERF-09). CI is `.github/workflows/rust.yml`, with every action pinned by full commit SHA.

## Engineering standard (rust-craft)

All Rust here follows rust-craft. The full text, with rule IDs, examples, the review rubric and the process, is the `rust-craft` skill shipped in the temper plugin. The non-negotiables for production code (tests may use `unwrap`/`expect`/`assert!`):

- H-01 no `unsafe`; H-02 no `unwrap`/`expect`; H-03 no `panic!`/`todo!`/`unimplemented!`/`unreachable!`, and no std API that panics on input the code does not control; H-04 no panicking indexing or slicing, including `map[&k]` and `deque[i]`, which no lint catches.
- H-05 no `#[allow]`; `#[expect(lint, reason)]` needs human approval; never weaken lint, CI or hook config.
- H-06 no `dbg!`, no print macros (use `tracing` or `writeln!` on a locked handle), no commented-out code, no untracked TODOs.
- H-07 no `Box::leak`/`mem::forget`/`transmute`/`static mut`/`'static` to silence lifetimes; H-08 no clones to appease the borrow checker.
- H-09 typed errors in library APIs (`thiserror`), with `anyhow` only in binaries; H-10 never discard failures; H-11 no lossy `as` casts.
- H-12 never weaken tests or checks, including manifest test switches (`test`, `doctest`, `harness`, `autotests`) and `#[ignore]` or `cfg_attr(.., ignore)` without a reason; H-13 bounded and owned resources.
- H-14 no blocking in async code, and no lock guard, `RefCell` borrow, entered tracing span or `watch::Ref` held across `.await`; H-15 zero warnings, documented public API.
- H-16 new dependencies and major-version bumps, `[patch]` overrides, `rust-version` raises, API breaks, lint exceptions, crate-boundary changes, migrations and test rewrites are human decisions.

Pre-approved dependencies are listed in `.claude/temper.json`; any other is an H-16 decision. Process: explore, frame, plan (`.temper/plans/`), red, green, refactor, self-review, gate, independent review, then report with a Status block (`DONE`, `WIP` or `BLOCKED`, plus the gate result, the review verdict, what changed, decisions needed and what was not verified).

## Decisions log

| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Six crates, dependencies inward, domain limited to serde, thiserror and sha2, no weighin-core or temper code. | ADR-07, P8, ARC-02 | owner (ADR-07) |
| 2026-10-11 | Every crate is private: `publish = false` in `[workspace.package]`, inherited by all members. | H-16 | owner |
| 2026-10-11 | Pre-approved dependencies: serde, serde_json, toml, thiserror, jsonschema, sha2, hex, clap, camino, tempfile, syn, regex, time, tracing, proptest, insta, assert_cmd. | H-16, ARC-12 | owner |
| 2026-10-11 | No `license` field in `[workspace.package]` (private code); `deny.toml` ignores unpublished workspace crates in the license check. | ARC-12 | Story #46, to confirm |
| 2026-10-11 | Root `CLAUDE.md` only imports this charter and adds Claude Code specifics; it repeats none of it (02 §2). | ARC-08 | Story #46 |
| 2026-10-11 | `thiserror` (library errors) and `tempfile` (anonymous spool files for git's stdio in `assay-forge`, and test fixture repositories) enter the workspace with the `assay-forge` git layer; both are pre-approved. | H-16, ARC-12 | Story #52 |
| 2026-10-11 | Unused layer edges are not declared up front: `cargo machete` rejects unused dependencies, so each story adds the edge it first uses. | CRF-05 | Story #46 |
| 2026-10-11 | Releases: `v*` tags (protected by ruleset) publish `assay` to GHCR as an OCI artifact with oras 1.3.4 (`oras-project/setup-oras`, SHA-pinned); consumers fetch and verify by digest with `.github/actions/fetch-by-digest`. Runbook `docs/operations/release.md`. `plugins.toml` parsing in Rust stays with #62; the action reads pins with python3 `tomllib`. | ADR-14, 05 T14 | Story #68 |
| 2026-10-11 | Pre-approved normal dependencies of `assay-app`'s contract validation: `jsonschema` 0.58 (default features off, so no HTTP/TLS or file resolution; schemas are embedded), `serde_json`, `thiserror` 2 (workspace requirement raised to 2.0.12). | H-16, ARC-12 | Story #47 |
| 2026-10-11 | Every contract document, `assay.run/1` and `assay.decision/1` included, carries its id in a `schema` field; 03 §4 and §5 do not list one, but the preamble requires versioned, validated contracts. | OUT-03 | Story #47, to confirm |
| 2026-10-11 | Schema interpretations where 03 is silent: `trigger` is not required by the schema (a missing one is disposition `no-trigger`, 04 §4); `expected_behavior` (omission) and `caller` (interaction) are; gate documents require `conclusion`; reviewer `source` requires `role`; a shard is `{files[]}`; for grouped event kinds (`GateStarted|Finished`, `Specialist*`) only `plugin`/`role` is required; `data` of kinds without listed fields is any object. | OUT-03 | Story #47, to confirm |
| 2026-10-11 | Narrow `deny.toml` exceptions for `jsonschema`'s tree: `ahash` and `wasm-bindgen` added as `once_cell` ban wrappers, and `MIT-0` allowed for `borrow-or-share` only (not in the global allow list). | H-16, ARC-12, MOD-03 | owner (Weigh-In wi-a01169, option A) |
