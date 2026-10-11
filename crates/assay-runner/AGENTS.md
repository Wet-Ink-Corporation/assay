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
- `ClaudeHeadless` (`new`, `with_max_output_bytes`): the `ModelRunner` that runs `claude -p` with the 03 §8 profile, built from the CLI's path, a `Workdir` (absolute, empty, checked again before every call) and a `HeadlessEnv` (the OAuth token plus explicitly passed variables).
- `Replay`, `Recording` (`Completed`, `TimedOut`): the `ModelRunner` over recorded transcripts, classified exactly as live ones.
- Errors: `HeadlessError`, `WorkdirError`, `EnvVarError`, `ReplayError`.

## Invariants
- Every spawned process has an owner that waits on or kills it, and a timeout (H-13).
- Output read from a process is size-capped (H-13): stdout is spooled to an anonymous temporary file and the call is killed past the cap (32 MiB by default); stderr is discarded.
- The argv is exactly 03 §8's, in its order (`tests/it/headless.rs` pins it flag for flag): `-p <prompt> --output-format stream-json --verbose --json-schema --append-system-prompt-file --exclude-dynamic-system-prompt-sections --restricted --safe-mode --settings <03 §8 JSON> --tools Read,Grep,Glob --add-dir --permission-mode dontAsk --max-turns --max-budget-usd <cap>`. Never `--bare`. The tool set is a constant, not a call field.
- The environment is cleared; only the OAuth token, `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` and passed variables reach the CLI, and `ANTHROPIC_*`, `CLAUDE_CODE_USE_*`, `AWS_*`, `GOOGLE_*`, `CLOUD_ML_*` and `VERTEX_*` cannot be passed.
- Cost comes only from the last `result` message's `total_cost_usd`; a timeout, a transcript past the cap, or a missing or unreadable cost is charged at the call's budget. Only `rate_limit_event`s with status `rejected` or `allowed_warning` are reported. The exit status is not consulted.
- The CLI is killed only at the timeout or the output cap, never on assay's own mid-run accounting (03 §8).

## Runtime prerequisites
- Unix (`process_group`). Only the CLI process itself is killed at a limit: the process-group killer lives in `assay-forge`, which this crate may not depend on (see Decisions).

## Errors
- `HeadlessError`: before the CLI starts (`RelativePath`, `Workdir`, `TimeoutTooLong`, `Spool`, `Spawn`) nothing is charged; after it (`Supervise`, `Kill`, or `Spool` on reading back) the cost is unknown and the caller charges the call's budget. `Kill` means the CLI may still be running: abort the job, do not retry.
- Model-side endings (budget, turns, timeout, quota, malformed output) are `ModelOutcome`s, not errors (ERR-10).

## Test strategy
Integration tests in `tests/it/` parse recorded stream-json transcripts (`tests/fixtures/stream/`, one per outcome) through `Replay`, and drive `ClaudeHeadless` against a stand-in script (`tests/fixtures/fake-claude`) that records its argv, cwd and environment; no test starts the real `claude` (TST-10). The timeout test waits on a 300 ms limit. Run: `cargo test -p assay-runner`.

## Do not
- Do not trust model output as authoritative; it is validated mechanically downstream (ADR-08).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | `ModelRunner` adapters added (#82), with edges to `assay-app`, `assay-domain`, `serde_json` (stream parsing by `Value`, no serde derive), `tempfile` (stdout spool) and `thiserror`, all pre-approved. | ARC-12, H-16 | #82 |
| 2026-10-11 | The CLI process is killed alone, not its process group: `assay-forge`'s `GroupKiller` cannot be reused across the adapter boundary (ADR-07), and moving it is a crate-boundary decision left to the owner. | H-13, H-16 | #82, pending owner |
