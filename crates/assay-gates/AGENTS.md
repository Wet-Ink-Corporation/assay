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
- `TemperAdapter<S, R>` (`new`; implements `assay_app::GateRunner` in `gates-static`), `TemperPaths` (`node`, `pinned`, `head_tree`), `TemperSettings` (`new(paths, environment, run_key)`, refusing a relative `node`, a relative or non-UTF-8 pinned copy, a relative or root head tree, a `PATH` with an empty or relative entry and an empty run key with `TemperSettingsError`), `TemperError`: the temper adapter's Z2s checks, 03 §2.1 steps 1-3 (`src/temper.rs`).
- `CaptureSupervisor` (implemented for `&mut S` too) and `CapturedRun`: a `Supervisor` that keeps stdout, for the pinned `temper scan` (`src/capture.rs`). Production: `assay_cli::ForgeSupervisor`.
- `RevisionFiles` (`list`, `read`, `blob_id`; implemented for `&R` too), `Revision`, `TreeEntry`, `EntryKind`: the PR's base and head trees as data, listed, read in bounded batches and compared by blob id (`src/revision.rs`). Production: `assay_cli::ForgeRevisions`, over forge's object-only repository; this crate does not depend on forge.

## Invariants
- Gate tools run as external processes with timeouts and size-capped output (H-13).
- A gate process sees only `PATH`, `HOME`, `CI`, `ASSAY_BASE_SHA`, `ASSAY_HEAD_SHA` and `ASSAY_RUN_KEY` (exact, case-sensitive names, 03 §2; no other `ASSAY_*` variable) plus the adapter's `ASSAY_FINDINGS_OUT`; an inherited `ASSAY_FINDINGS_OUT` never passes the allowlist.
- The checkout and the output root are absolute paths (`ExecSettings::new`), so neither depends on this process's working directory.
- `exec` runs only in its own Z2b job: `gates-static`, another Z2b job or a non-`exec` plugin is refused before anything runs.
- Each run writes into `<output_root>/<plugin>/`, created fresh; an existing directory is refused, so a stale document is never read. The document is read only when it is a regular file (never through a symlink) of at most `max_document_bytes`.
- The temper adapter runs only in `gates-static`, and only the base's pinned temper: `<node> <pinned>/temper.mjs scan --all --json` from the head tree, `<node>` absolute, with the `GateEnvironment` (its `PATH` absolute entries only) plus `GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CEILING_DIRECTORIES` (the head tree's parent). Nothing under the PR's `.claude/temper/` runs or is read; its drift from the pinned copy, by blob id, is a `major` `vendored-drift` finding that does not decide the gate (05 T4).
- What the scan's walk cannot see fails as `blind-spot`: `.rs` under `target`, `node_modules`, `.git` or `.temper` at any depth, submodules, symlinks that do not resolve lexically inside the tree through real directories to a regular file (`.rs` for a `.rs` link), unlexable `.rs`, and (from `src/temper/tokens.rs`, a lexer that drops comments and literals) `cfg_attr` naming `allow`, `expect`, `path` or a `$` fragment, and `#[path]`. An `#[expect]` is admitted only when every `#[expect]` at the reported line names exactly one admitted lint, and an `#[expect]` the scan did not report fails.
- Fail closed: a baseline or `.claude/temper.json` missing, not a regular file or unparsable on either side, an unparsable base `.assay/exceptions.toml`, a head tree past the reader's limits, a scan that does not exit 0 or 1 or prints unparsable output, and an unreadable pinned copy are `TemperError`s, never a pass. Configuration (the exceptions allowlist) is read from base only. An absent `.assay/exceptions.toml` admits nothing.
- The adapter's own document always satisfies `findings-1.schema.json`: text is clipped to the schema's caps, and PR-controlled names in titles and bodies sit in code spans, reduced to `[A-Za-z0-9_:-./]` with `://` replaced. Parse errors of PR-controlled files never quote them.

## Errors
`ExecError<E>` (`NotExec`, `OutsideBuildJob`, `WrongJob`, `OutputDirectory`, `Supervisor(E)`, `ReadDocument`): the gate could not run, recorded by the use case as `gate-error:not-run`. A gate that ran and failed is a `GateRun`, never an error (ERR-10). `ExecSettingsError` (`RelativeCheckout`, `RelativeOutputRoot`): the composition root passed a relative directory. `TemperError<S, R>` (`NotTemper`, `OutsideStatic`, `Revision(R)`, `Missing`, `NotAFile`, `Malformed`, `Supervisor(S)`, `ScanEnded`, `ScanOutput`, `Pinned`, `BlobId(R)`, `Document`): the temper gate errors (`gate-error:not-run`). `TemperSettingsError` (`RelativeNode`, `RelativeSearchPath`, `RelativePinned`, `NonUtf8Pinned`, `HeadTree`, `EmptyRunKey`).

## Known gaps
- Escaping the group kill: a gate is PR-controlled code, and a process it starts can leave the gate's process group with `setsid` or `setpgid`. The supervisor kills and waits for the group only, so such a process is neither killed at the deadline nor waited for, and can outlive the gate run (and write into the output directory afterwards). Only the job's VM or container boundary contains it; closing it needs a cgroup or PID-namespace supervisor (H-16, #150 review F5).
- FIFO swap (`read_document`): the document's type check and its open are two calls. The supervisor has shown the gate's group gone first, so only a process that left the group (`setsid`) could swap a FIFO in between, which would block the open. Closing it needs `O_NOFOLLOW | O_NONBLOCK`, which std cannot name without `libc` (H-16; #152 F8).
- temper's Z2b part (`baseline --check` in `gates-lint`, 03 §2.1 steps 5-6) and the Z2s static checks of step 4 are not built: the temper adapter refuses every job but `gates-static` (`OutsideStatic`), so a required temper gate is `gate-error:not-run` in `gates-lint` until they are.
- `raised` compares crate names as the baselines spell them: a crate renamed in the PR makes its entries new, which fails (closed) where temper would carry the counts over.
- The scan reads the head tree from disk: the job must materialise exactly the head commit there, outside any other git repository or Cargo workspace (temper's project discovery climbs to the outermost `Cargo.toml`).
- Blind spots not closed by the Z2s checks (#63 review F5): `include!`/`include_str!` of a non-`.rs` file and Cargo's `build = "build.txt"` or `[lib] path`/`[[bin]] path` pointing at a non-`.rs` file compile text the scan never reads (manifest checks are step 4b's); attributes assembled by a `macro_rules!` fragment outside `cfg_attr` (`#[$m]`, common for doc forwarding) and by proc macros are not read; and the attribute scan fails closed on harmless forms too: a `cfg_attr` forwarding any `$x` fragment, an identifier `allow`, `expect` or `path` anywhere in a `cfg_attr`'s arguments (`cfg_attr(feature = "x", derive(path))`), and a hand-written lexer's disagreements with rustc, which can only show it more code. The `.rs` files of one head are read in one batch, so a head with more Rust than the reader's byte limit (32 MiB in `assay-cli`) is a gate error rather than a partial check.
- An `[[exception]]` admits a covered (non-attribute) hit by (file, lint), so one admitted lint covers every hit temper attributes to it in that file; the attribute's own hit is what binds the exception to a single-lint `#[expect]`.

## Test strategy
Integration tests in `tests/it/` parse recorded tool outputs; missing contract surface is requested upstream, not reverse-engineered. The exec adapter is tested against a scripted `Supervisor` that plays the gate process (`tests/it/support.rs`). The temper adapter is tested against in-memory base and head trees (`FakeRevisions`, blob ids from a hash of the bytes) and a scripted scan whose hits model temper 1.3.0's `scan --all --json` (`tests/it/temper_support.rs`); every outcome goes through `run_gates`, so its document is schema-checked. End-to-end fixture PRs (git repositories) are in `assay-cli` (`tests/it/temper.rs`). Run: `cargo test -p assay-gates`.

## Do not
- Do not link or vendor temper or Weigh-In code to read their state.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | Adds the `assay-app` edge and `thiserror` with the `exec` adapter. The process-group supervisor in `assay-forge` is not reachable from here (forbidden edge, and `pub(crate)`), so the adapter is generic over `Supervisor`; where the real supervisor lives is an open crate-boundary decision. | ARC-02, H-13, H-16 | Story #62, owner to decide |
| 2026-10-12 | Decided: forge's supervisor is made public and `assay-cli` implements `Supervisor` with it (`ForgeSupervisor`); no new edge from this crate. | ADR-07, H-16 | owner, Weigh-In wi-603c63 (decided A), #150 |
| 2026-10-12 | #152 review follow-ups: only the three `ASSAY_*` variables 03 §2 names pass (F7), `ExecSettings` is built by a validating `new` with private fields (F2, an API change inside the workspace), `WrongJob` displays the job as `gates-lint`/`gates-test` (F10), and the FIFO gap is recorded above (F8). | H-13, API-03, H-16 | #152, #150 |
| 2026-10-12 | temper adapter, Z2s (#63). Adds the `assay-domain` edge (the findings document), `serde`, `serde_json` and `toml` (pre-approved, already workspace dependencies). The PR's files come through a port this crate owns (`RevisionFiles`) and the scan's stdout through `CaptureSupervisor`, an additive sibling of `Supervisor`; both are implemented in `assay-cli` over forge, following wi-603c63, so no edge to forge. | ADR-07, ARC-02, OUT-01 | #63 |
| 2026-10-12 | `.assay/exceptions.toml` format (03 §1): `[[exception]] path, lint, reason` and `[[gate_config]] key, value, reason`, where `value` binds the exact head value. Vendored drift is a `major` finding that does not fail the gate (03 §2.1 and T4 say "finding"). An absent exceptions file admits nothing. | T4, T5, H-16 | #63, owner to confirm |
| 2026-10-12 | #63 review: `[[gate_config]]` binds (key, `base`, `head`) instead of `value` (F7); `RevisionFiles` becomes `list`/`read`/`blob_id` over bounded forge reads, and vendored drift compares blob ids (F1, F9); `TemperSettings::new` takes `TemperPaths` with an absolute `node` and refuses a relative `PATH` entry (F3); single-lint `#[expect]` only (F4); `blind-spot` findings and a Rust lexer (F5); the baseline's `crates` map is diffed, removal allowed (F6); names in code spans, `://` replaced (F8). All are API changes inside the workspace on an unmerged PR. | H-13, H-16, API-03 | #63 review, coordinator |
