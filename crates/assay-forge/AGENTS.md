# Crate charter: `assay-forge`

## Purpose
The GitHub adapter: REST and GraphQL through `gh`, App-token minting narrowed per job, diffs and objects, checks (compare-and-swap by check id and `external_id`), comments, timeline events, reactions and the ledger repository. It implements the `Forge` and `Ledger` ports of `assay-app`. It is its own crate because it is the only code that talks to GitHub and decides who counts as an owner (ARC-07, `docs/architecture/02-architecture.md` §2, §7).

## Layer
adapter. Implements application ports; wire DTOs for GitHub live here and are converted with `TryFrom` into domain types (OUT-01). No business rules.

## Allowed dependencies
- Workspace crates: `assay-app` and `assay-domain`, added by the story that first uses them.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added.
- Never: `assay-runner`, `assay-gates`, `assay-cli`, `weighin-core` or temper code (P8). `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
Workspace-internal, not published (`publish = false`).
- `git`: the hardened git layer for PR objects (`docs/architecture/03-contracts.md` §9, §2.1). `ObjectRepo` (bare, object-only repository: `init`, `fetch` by `CommitSha` from an `HttpsRemote`, `numstat` under a wall-clock cap returning `DiffOutcome::{Measured, NeedsSplit}`, `police_attributes`), `GitLimits`, `is_policed_path`, and the errors `GitError`, `AttributeError`, `ProcessError`, `ParseShaError`, `ParseRemoteError`.

## Invariants
- Every external call has a timeout and a bounded response size (H-13).
- Every git process is built by `git::command::git`, the only `Command::new` in production code (`tests/it/single_constructor.rs` enforces it). It clears the environment to `PATH` and the proxy variables, sets `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_ATTR_NOSYSTEM=1`, and passes the §9 `-c` flags; every diff carries `--no-ext-diff --no-textconv`.
- PR objects are fetched only by full SHA over https into a bare repository; refs such as `refs/pull/N/merge` cannot be expressed.
- Every child process is waited on; one past its deadline is killed first. A diff past its cap is `needs-split`, never a hang.
- Attribute policing needs git 2.40 or later (`check-attr --source`); an older git fails closed.

## Errors
A `thiserror` enum per operation, separating GitHub rejections from transport failures (ERR-10). git: `GitError` (process failure, non-zero exit, unreadable output, missing commit) and `AttributeError` (`Forbidden`, displayed as `gate-error:gitattributes`, or a `GitError`); both fail closed.

## Test strategy
Integration tests in `tests/it/` against recorded `gh` responses; no live network in tests (TST-10). The git layer is tested against fixture repositories built with plain git in a temporary directory (`tests/it/support.rs`); fetch is covered by its argument list, not by a network fetch. Run: `cargo test -p assay-forge`.

## Do not
- Do not decide verdicts or severities here; that is the domain's job.
- Do not hold a write token longer than the operation needs.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | The hardened git layer lives here (the charter's "diffs and objects"), not in the domain (I/O). Processes are bounded with std scoped threads and polling, not a timeout crate (none is pre-approved). | ARC-02, H-13, H-16 | Story #52 |
