# Crate charter: `assay-cli`

## Purpose
The `assay` binary: the composition root. It parses arguments, loads and validates configuration once, wires the adapters into the use cases and maps outcomes to exit codes (`docs/architecture/02-architecture.md` §2). It also hosts the workspace's architecture tests, because it is the one crate that may depend on every other.

## Layer
binary. Thin (ARC-14): no business logic, nothing a test would want to call directly.

## Allowed dependencies
- Workspace crates: all five others, added by the story that first wires each.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added. Dev only: `serde_json` (parses `cargo metadata` in the architecture test), `tempfile` (scratch directories in the supervisor tests).
- Wired so far: `assay-app`, `assay-forge`, `assay-gates`, `assay-runner` (#150); `assay-domain` (#55, for `ForgePullRequest`'s `FileChange`; a dev-dependency before).
- Never: `weighin-core` or temper code (P8).

## Public surface
The `assay` command line. A library target, `assay_cli`, holds the adapters the composition root lends one crate from another, so they are tested before a command wires them: `ForgeSupervisor` (`new`), `assay-forge`'s process-group supervisor as an `assay_gates::Supervisor`, an `assay_gates::CaptureSupervisor` (stdout kept; none after a timeout or an over-cap run) and an `assay_runner::GroupKill` (wi-603c63); and `ForgeRevisions` (`new(repo, base, head, limits)`), forge's object-only repository as the temper adapter's `assay_gates::RevisionFiles` (#63), every listing and read bounded by forge's `TreeLimits`; to read blobs it lends each requested entry's path and object id to forge as a borrowed `BlobRef`, copying nothing; and `ForgePullRequest` (`new(repo, limits, diff_cap)`, error `ForgeObjectsError::{Git, Commit}`), the same repository as `assay_app::PullRequestObjects` for `plan` (#55): forge's `changed_files` mapped to the domain's `FileChange` (a gitlink on either side is `Gitlink`, else a symlink `Symlink`; an unmeasured count `Binary`), and its tree listings and reads. Nothing else is public. As a `Supervisor` it discards the gate's output, but forge reads it back first: a gate run briefly holds up to twice `max_output_bytes` in memory (32 MiB at `ExecLimits::DEFAULT`), accepted because one gate runs at a time.

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
- `exec_surface.rs`: exec-surface detection end to end (04 §2.2, #65) on fixture PRs built as git repositories and fetched into an `ObjectRepo`: forge's manifest copy, the real cargo that built the tests as the pinned cargo (`PinnedCargo` under `ForgeSupervisor`) with registry crates from a directory source in the test's own `CARGO_HOME` (no network), and `assay_app::exec_surface`. A new `build.rs` and a bumped crate gaining a build script are exec surface, a plain registry bump is only `Registry`, a path build-dependency inherited through `[workspace.dependencies]` is an exec root, a symlink in a build-time crate leading outside it (a `.cargo/` one the copy never writes included) and a build-dependency's `[lib] path` outside its crate are `Everything` (cargo leaves `..` in that `src_path`), an unresolvable `workspace = true` fails closed, and the `rustc` and `rustc-wrapper` of a PR's root `.cargo/config.toml` never run (the wrapper, outside the tree, would leave a marker). The test's cargo is not a rustup proxy, so it proves nothing about `rust-toolchain.toml`; that the copy holds no toolchain file, nested `.cargo` or `.gitattributes` is pinned by forge's `manifest_copy` tests. Unix only; needs git.
- `fetch_by_digest.rs`: `.github/actions/fetch-by-digest/fetch-by-digest.sh` against a fake `oras` (no network): a digest, artifact-type, media-type or size mismatch aborts with nothing installed, malformed repositories and out-dirs are usage errors, and the token travels on stdin (ADR-14, 05 T14). Unix only; needs `bash`, `jq`, `sha256sum` and `python3` >= 3.11.
- `plan.rs`: `plan` end to end (#55) on fixture PRs whose base moved on after the fork, fetched into an `ObjectRepo` and read through `ForgePullRequest`: the plugins come from the base config even when the head rewrites it (a trust-path change), only the head's own files are listed (merge base), a REST list git does not confirm fails the plan, and a submodule under base `.assay/` is refused. Needs git.
- `release_workflow.rs`: `.github/workflows/release.yml` runs only on `v*` tags with least-privilege jobs, and every action in `.github/` is pinned by commit SHA.
- `warm_target_workflow.rs`: `.github/workflows/warm-target.yml` (#69, ADR-21) runs only on pushes to `main` with least-privilege jobs, builds at the fixed path `/work/ws` (A-29) and cleans the workspace crates before packing; `publish` runs no repository code; the restore action fetches by digest, unpacks, then cleans.
- `warm_target.rs`: `.github/actions/warm-target/warm-target.sh` with fake `zstd` and `cargo` (nothing compiles, no network): pack and unpack round-trip `target/` and the registry index and cache (not `registry/src`); an entry outside that allowlist, a link, a special file, a `..`, empty or `.` component, an absolute or control-character name, or a `target` that links out of the workspace refuses the archive with nothing written; `clean` runs one `cargo clean --locked -p` per workspace package and refuses a name that is not a cargo package name. Unix only; needs `bash`, `jq` and `python3` >= 3.12 (tarfile filters).
- `workflow_cache.rs`: no decision-bearing job uses the Actions cache (ADR-21, 05 T6). Allowlists both ways (wi-b5affa): a job is decision-bearing unless exempted by name (`rust.yml`'s `gate`, `aarch64`, `miri`), and such jobs and every composite action may use only listed cache-free actions; a `uses:` the line reader cannot judge is a finding. `workflow.rs` holds the line readers shared with `release_workflow.rs`.
- `supervisor.rs`: `ForgeSupervisor` with the system `kill`: a gate's exit code, signal, timeout (its whole group gone), output cap, working directory and exact environment; and `ClaudeHeadless` with it against a stand-in `claude` that leaves a helper in its group, which must be dead after a timeout and after a clean exit. Linux only (`/proc`, procps or util-linux `kill`).
- `temper.rs`: the temper adapter's Z2s checks end to end on fixture PRs built as git repositories and fetched into an `ObjectRepo`, read through `ForgeRevisions`, with a stand-in `node` at an absolute path (run by `ForgeSupervisor`) that accepts only the pinned invocation and prints a recorded scan, `tests/fixtures/temper-scan-expect.json` or `temper-scan-expect-two-lints.json`, modelled on temper 1.3.0's `scan --all --json` (which names only an `#[expect]`'s first lint). A raise, an unallowlisted `#[expect]`, a two-lint `#[expect]` whose first lint is allowlisted and gate-config drift fail; a pure decrease and an allowlisted single-lint `#[expect]` pass; vendored drift is reported. The real temper never runs. Unix only; needs git.

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
| 2026-10-12 | `ForgePullRequest` lends forge's object-only repository to the `plan` use case (#55), as `ForgeRevisions` does to the temper adapter, rather than forge implementing the app port itself: no new crate edge into forge. `assay-domain` moves from a dev-dependency to a dependency for it. The `assay plan` subcommand waits for its inputs (the REST file list, #53; the event and job outputs, #60). | ADR-07, ARC-03 | #55 |
| 2026-10-12 | Warm `target/` (#69): the cache rule exempts `rust.yml`'s `gate`, `aarch64` and `miri` (they keep `Swatinem/rust-cache`) because 06 §3 makes the repository's own workflow developer feedback that assay re-runs from base config; every other job, present or future, is held to it. The restore lives in a composite action plus script, tested here, because the Z2b workflow template does not exist yet. | ADR-21, wi-b5affa | #69; owner may reopen the exemption |
| 2026-10-12 | `ForgeRevisions` lends forge's object-only repository to the temper adapter, and `ForgeSupervisor` implements `CaptureSupervisor` for its scan (#63); same pattern, no new edge. | ADR-07 | #63 |
