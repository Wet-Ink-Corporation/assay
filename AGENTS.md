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

`docs/architecture/02-architecture.md` §7. `schemas/` holds the contract JSON Schemas and their golden fixtures (its README indexes them). `plugin/prompts/<version>/` holds the reviewer prompt bundles; their hash, computed by `assay_app::PromptBundle`, is part of the evidence key. Directories without content yet (`plugin/`, `workflows/`, `rubric/`, `corpus/`, `docs/decisions/`) carry a README; `crates/assay-cli/tests/it/layout.rs` checks them.

**Prompt and rubric changes.** A PR that changes `plugin/prompts/**` or `rubric/**` changes what reviewers report. Once M5 exists, such PRs run the corpus replay gate (`docs/architecture/04-pipeline.md` §8, `06-operations.md` F11): k = 3 runs per `dev` case, accepted only when no case flips from detected to missed and the noise rate does not rise beyond its margin. Until then they are trust-path changes reviewed by the owner, and the PR says that the replay gate has not run.

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
| 2026-10-11 | `sha2` (finding fingerprints in `assay-domain`, no default features) and `proptest` (dev-dependency for property tests, `std` only) enter the workspace with the fingerprint; both are pre-approved. | H-16, ARC-12 | #49 |
| 2026-10-11 | `toml` (`std`, `parse`, `serde`; bounded nesting kept) enters the workspace, and `assay-app` takes it and the workspace `serde` entry (Story #48, same `std` and `derive` features), to parse base `.assay/plugins.toml`; both are pre-approved. `assay-gates` gains its `assay-app` edge with the `exec` adapter. | H-16, ARC-12 | Story #62 |
| 2026-10-11 | `sha2` (decision digests in `assay-domain`, and the fingerprints of #49) and the test-only `proptest` enter the workspace with rubric v1 and `decide`; `assay-domain` also takes the workspace `toml` entry (Story #62) as a dev-dependency only, to read `rubric/v1.toml` in tests. All are pre-approved. | H-16, ARC-12 | Story #51 |
| 2026-10-12 | `syn` (pre-approved; `parsing` only) enters the workspace as an `assay-gates` dev-dependency, to check the temper adapter's lexer against proc-macro2's; syn 2 was already in the lockfile, so no crate is added. | H-16, ARC-12 | #63 review round 2 |
| 2026-10-12 | T16 (#76): base `.assay/globs.toml` makes 02 §7's paths and `corpus/golden/**` trust paths of this repository (the corpus addition is the fail-closed reading of "owner-approved snapshot update": the snapshot update is what rule 4 holds for the owner); the `assay-self` gate is `assay self-gate`, an `exec` plugin documented in 02 §7, declared in `.assay/plugins.toml` once a released assay digest exists. | T16, wi-b5affa, H-16 | #76; owner to confirm |
| 2026-10-12 | Warm `target/` (#69, ADR-21): `.github/workflows/warm-target.yml` builds on pushes to `main` at `/work/ws` and publishes `target/` with the registry cache to GHCR (`assay-warm-target`) by digest, `packages: write` on `publish` only; Z2b restores it with `.github/actions/warm-target` (needs `python3` >= 3.11.4). No decision-bearing job uses the Actions cache (`crates/assay-cli/tests/it/workflow_cache.rs`). Runbook `docs/operations/warm-target.md`. | ADR-21, 05 T6, H-16 | owner |
| 2026-10-11 | Unused layer edges are not declared up front: `cargo machete` rejects unused dependencies, so each story adds the edge it first uses. | CRF-05 | Story #46 |
| 2026-10-12 | Prompt bundle v1 (`combined`, `combined-micro`) in `plugin/prompts/v1/`, read by the caller (the app does no file I/O) and hashed in the domain (the only crate with `sha2`). The evidence key itself is composed by `plan` (#55), which takes `PromptBundle::hash` as `prompt_bundle_hash`. Prompt and rubric PRs run the corpus replay gate once M5 exists. | ARC-02, 04 §3, 04 §8 | #90 |
| 2026-10-11 | Releases: `v*` tags (protected by ruleset) publish `assay` to GHCR as an OCI artifact with oras 1.3.4 (`oras-project/setup-oras`, SHA-pinned); consumers fetch and verify by digest with `.github/actions/fetch-by-digest`. Runbook `docs/operations/release.md`. `plugins.toml` parsing in Rust stays with #62; the action reads pins with python3 `tomllib`. | ADR-14, 05 T14 | Story #68 |
| 2026-10-11 | `serde` (features `std`, `derive`; pre-approved and already in the lockfile through `serde_json`) enters the workspace for the contract types in `assay-domain`, one of the domain's three allowed dependencies (02 §2); `serde_json` is the domain's dev-dependency for fixture round-trips. | H-16, ARC-12 | Story #48 |
| 2026-10-11 | Pre-approved normal dependencies of `assay-app`'s contract validation: `jsonschema` 0.58 (default features off, so no HTTP/TLS or file resolution; schemas are embedded), `serde_json`, `thiserror` 2 (workspace requirement raised to 2.0.12). | H-16, ARC-12 | Story #47 |
| 2026-10-11 | Every contract document, `assay.run/1` and `assay.decision/1` included, carries its id in a `schema` field; 03 §4 and §5 do not list one, but the preamble requires versioned, validated contracts. | OUT-03 | Story #47, to confirm |
| 2026-10-11 | Schema interpretations where 03 is silent: `trigger` is not required by the schema (a missing one is disposition `no-trigger`, 04 §4); `expected_behavior` (omission) and `caller` (interaction) are; gate documents require `conclusion`; reviewer `source` requires `role`; a shard is `{files[]}`; for grouped event kinds (`GateStarted|Finished`, `Specialist*`) only `plugin`/`role` is required; `data` of kinds without listed fields is any object. | OUT-03 | Story #47, to confirm |
| 2026-10-11 | Narrow `deny.toml` exceptions for `jsonschema`'s tree: `ahash` and `wasm-bindgen` added as `once_cell` ban wrappers, and `MIT-0` allowed for `borrow-or-share` only (not in the global allow list). | H-16, ARC-12, MOD-03 | owner (Weigh-In wi-a01169, option A) |
| 2026-10-11 | Schemas follow 03 v1.2. Shapes where it is silent: `run_uid` is `{repo_id, run_id, run_attempt}` of positive integers (02 §5; `run_attempt == 1` is enforced by the binary, not the schema); `budget.tier_degraded_from` is `full` only (04 §2.4); `budget.reused_evidence` is 02 §5's evidence record `{evidence_key, bundle_sha256, llm, tier}` with `skipped(draft)` written `skipped`; `budget.retry_grant` is `{role}`; event ids `sender_id`, `actor_id`, `comment_id` are positive integers and `command` is free text. Tests pin the schemas' reason codes and event kinds to 03's lists. | OUT-03 | owner (Weigh-In wi-7471b2, option A) |
| 2026-10-12 | Contract reads refuse a document over `MAX_DOCUMENT_BYTES` (8 MiB, the exec gate's default document cap) unparsed, and any object that repeats a key, through a `serde` visitor in `assay-app` (a direct dependency since Story #62; no new dependency). `run-1` requires `tier: lite` when `budget.tier_degraded_from` is present (03 §4). Tests also pin the domain's disposition codes to 03 §5 and each closed event kind's `data` fields to 03 §6. | OUT-05, H-13, TST-05 | #145 |
| 2026-10-12 | New contract `assay.run-inputs/1` (`schemas/run-inputs-1.schema.json`, 03 §6a): the bundle `decide` persists, one JSON file whose sha256 is the evidence pointer's; refused runs persist none; the GitHub state snapshot is #58's. No override neutralises `base-not-protected` (R3-03). No new dependency. | OUT-03, H-16 | #57; two-way defaults, owner may reopen |
| 2026-10-12 | `sha2` enters `assay-app` with `decide`, which authenticates each artifact by the sha256 its producing job published; pre-approved. | H-16, ARC-12 | Story #56 |
| 2026-10-13 | Publication sanitiser (#175 backstop): model prose is published with every ASCII punctuation character escaped, `@`/`#` fullwidth and `://`/`www.` defanged (selective escaping, not prose inside a code block); only publisher-built repository permalinks are live and only publisher fences with allowlisted info strings exist. Inline comments and per-gate checks go through a new `CommentForge` port, tested against a fake until the GitHub adapter (#53) exists. No new dependency. | wi-b5affa, H-16 | #88; owner to confirm (#175 question) |
| 2026-10-13 | Local events and status (#130): `assay.status/1` (unreleased) gains its `local` branch in place rather than a new version, enforced by `if`/`then` on `source` (local: `tree`, `local_tree`, no repo/PR/viewer/url fields, findings never waivable). `CheckPorts` gains an `EventSink` port and `CheckError` its `Events`/`Clock` variants; public fields change type: `LocalGate.outcome` (`String` to `LocalOutcome`), `LocalGate.findings` and `LocalReview.findings` (`Vec<Finding>` to `Vec<LocalFinding>`); the local report becomes `assay.local-check/2` and a version-1 report is refused as an older format. Stale results are found through a `latest` file next to the reports. `--events` refuses an existing path; writes capped at 64 KiB per line and 16 MiB per stream. CI reads neither. No new dependency. | API-04, H-16, OUT-03 | #130; two-way defaults, owner may reopen |
| 2026-10-14 | Re-review carry set persists in the bundle (#224, superseding #99's caller-supplied carry set): `assay.run-inputs/1` (unreleased) gains a required `carry` in place, `{findings[{fp, file, line, end_line?, class, severity, verdict?, title, trigger?, evidence}] (at most 256), omitted[{fp, reason: cap\|refused}]}`; `next_carry` builds it from the run's settled carried findings and its standing findings, `publish` writes it (`Decided.carry`), and `prior_carry` seeds the next run from the previous completed run's bundle after its pointer's sha256, run and strict parse check, only when the carry-over is kept. `CarriedParts`/`CarriedFinding` gain `end_line` and `verdict` (workspace-internal API change). Rejected: rebuilding from published comments; accepting the one-hop drop. No new dependency. | OUT-03, H-16 | owner (#224, bundle) |
