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
Workspace-internal, not published (`publish = false`). Re-exported from `lib.rs`:
- `ClaudeHeadless<K>` (`new`, `with_max_output_bytes`): the `ModelRunner` that runs `claude -p` with the 03 §8 profile, built from the CLI's path, a `Workdir` (absolute, empty, checked again before every call), a `HeadlessEnv` (the OAuth token plus explicitly passed variables) and a `K: GroupKill`.
- `GroupKill` (`kill_led_group` while the CLI is unreaped, `kill_group` after; implemented for `&K` too): the process-group kill the composition root lends; `assay-cli` implements it with `assay-forge`'s `GroupKiller` (wi-603c63).
- `Replay`, `Recording` (`Completed`, `TimedOut`): the `ModelRunner` over recorded transcripts, classified exactly as live ones.
- Errors: `HeadlessError`, `WorkdirError`, `EnvVarError`, `ReplayError`.

## Invariants
- Every spawned process has an owner that waits on or kills it, and a timeout (H-13).
- The CLI leads its own process group, which the `GroupKill` kills whole: at the timeout or the cap with `kill_led_group` before the CLI is reaped (so its pid, the group id, cannot have been reused), and after the CLI exits on its own with `kill_group`, so no process it started outlives the call. A call returns only once the group is shown gone, or with `HeadlessError::Kill`; the CLI itself is killed and reaped even when its group kill fails.
- Output read from a process is size-capped (H-13): stdout is spooled to an anonymous temporary file and the call is killed past the cap (32 MiB by default); stderr is discarded.
- The prompt reaches the CLI on stdin, from an anonymous temporary file written before the CLI starts, never as an argument (wi-9fe37e).
- The argv is exactly 03 §8's, in its order (`tests/it/headless.rs` pins it flag for flag): `-p --output-format stream-json --verbose --json-schema --append-system-prompt-file --exclude-dynamic-system-prompt-sections --restricted --safe-mode --settings <03 §8 JSON> --tools Read,Grep,Glob --add-dir --permission-mode dontAsk --max-turns --max-budget-usd <cap>`. Never `--bare`. The tool set is a constant, not a call field.
- The environment is cleared; only the OAuth token, `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` and passed variables reach the CLI, and `NODE_OPTIONS`, `CLAUDE_CONFIG_DIR`, `ANTHROPIC_*`, `CLAUDE_CODE_USE_*`, `AWS_*`, `GOOGLE_*`, `CLOUD_ML_*`, `VERTEX_*`, `LD_*` and `DYLD_*` cannot be passed.
- Cost comes only from the last `result` message's `total_cost_usd`; a timeout, a transcript past the cap, or a missing or unreadable cost is charged at the call's budget, and a cost too large for `Aeu` saturates. The `result` decides the outcome: budget, turn and quota endings stay final even when a stray stdout line is unreadable, which only stops a success from counting (it becomes `Malformed`). Only `rate_limit_event`s with status `rejected` or `allowed_warning` are reported. The exit status is not consulted.
- The CLI is killed only at the timeout or the output cap, never on assay's own mid-run accounting (03 §8).

## Runtime prerequisites
- Unix (`process_group`), and whatever the lent `GroupKill` needs: in production `assay-forge`'s, a Linux `/proc` describing this process and a procps or util-linux `kill` (`docs/architecture/06-operations.md` §5 F22).
- Known gap: the CLI is stopped by assay, so it outlives an assay that is itself killed by a signal (`SIGKILL`, or a default-action `SIGTERM` or `SIGINT`): its own group does not receive signals sent to assay's group, and nothing in this process runs to kill it. It then runs until its own `--max-turns` or `--max-budget-usd` stop, or until the runner VM is torn down. Closing this needs a parent-death signal (`PR_SET_PDEATHSIG`: `unsafe` or a new dependency, or a `setpriv --pdeathsig` wrapper as a new runtime prerequisite), an H-16 decision.

## Errors
- `HeadlessError`: `may_have_run()` says whether to charge. Before the CLI starts (`RelativePath`, `Workdir`, `TimeoutTooLong`, `SpoolCreate`, `Spawn`) it is `false` and nothing is charged; after it (`Supervise`, `Kill`, `ReadBack`) it is `true`, the cost is unknown and the caller charges the call's budget. `Kill` means the CLI or a process in its group may still be running, even after a CLI that exited cleanly: abort the job, do not retry.
- Model-side endings (budget, turns, timeout, quota, malformed output) are `ModelOutcome`s, not errors (ERR-10).

## Test strategy
Integration tests in `tests/it/` parse recorded stream-json transcripts (`tests/fixtures/stream/`, one per outcome) through `Replay`, and drive `ClaudeHeadless` against a stand-in script (`tests/fixtures/fake-claude`) that records its pid, argv, stdin, cwd and environment; no test starts the real `claude` (TST-10). A recording `GroupKill` (`tests/it/support.rs`) pins which group is killed and whether the CLI was still unreaped then; that the group really dies, helpers included, is tested with the forge killer in `assay-cli` (`tests/it/supervisor.rs`). The timeout test runs a real 300 ms limit but asserts only the outcome and the charge, not how much the stand-in wrote before the kill. Run: `cargo test -p assay-runner`.

## Do not
- Do not trust model output as authoritative; it is validated mechanically downstream (ADR-08).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | `ModelRunner` adapters added (#82), with edges to `assay-app`, `assay-domain`, `serde_json` (stream parsing by `Value`, no serde derive), `tempfile` (stdout spool) and `thiserror`, all pre-approved. | ARC-12, H-16 | #82 |
| 2026-10-11 | Known gap (closed 2026-10-12 by #150, see below): at a limit the CLI process is killed alone, not its process group, so a helper it started can outlive it briefly (no process-spawning tool is available, so only the CLI's own helpers can be left). And because the CLI leads its own group (`process_group(0)`), signals sent to assay's group do not reach it: if assay dies, the CLI outlives it. `assay-forge`'s `GroupKiller` cannot be reused across the adapter boundary (ADR-07). Planned fix (option B), which covers both: make `GroupKiller` public in `assay-forge` and have `assay-cli` inject a group kill into `ClaudeHeadless`. | H-13, H-16 | Weigh-In default wi-469213 (two-way), owner may reopen |
| 2026-10-11 | The prompt goes to `claude -p` on stdin from a temporary file, not as an argument: one argument is capped at 128 KiB, which a context pack can exceed. 03 §8 updated; `assay doctor` (#84) must verify the pinned CLI reads it. | H-13, H-16 | owner, Weigh-In wi-9fe37e (decided A) |
| 2026-10-12 | wi-469213 closed: `ClaudeHeadless<K>` takes a `GroupKill` (a trait here, so this crate stays off `assay-forge`), which `assay-cli` implements with forge's `GroupKiller`. The CLI's whole group is killed at the timeout and the cap, and swept after the CLI exits, so nothing it started outlives a call. Not closed: an assay killed by a signal (Runtime prerequisites, known gap). `ClaudeHeadless::new` gained the killer argument (API change inside the workspace). | ADR-07, H-13, H-16 | owner, Weigh-In wi-603c63 (decided A), #150 |
