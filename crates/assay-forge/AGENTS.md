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
- `git`: the hardened git layer for PR objects (`docs/architecture/03-contracts.md` §9, §2.1). `ObjectRepo` (bare, object-only repository: `init` into an absent or empty directory, `fetch` by `CommitSha` from an `HttpsRemote`, `numstat` under a wall-clock cap returning `DiffOutcome::{Measured, NeedsSplit}`, `police_attributes` over every changed path with an `AttributeAllowlist`, returning an `AttributeReport` whose `opaque` entries downstream gates must treat as fully changed), `GitEnvironment` (the inherited variables and the `GroupKiller`, captured once at the edge), `GroupKiller`, `GitLimits`, `PolicedAttribute`, `is_protected_path`, and the errors `GitError`, `AttributeError`, `GlobError`, `KillerError` (with its `ProcFault` cause), `ProcessError`, `ParseShaError`, `ParseRemoteError`. Parsing the allowlist out of base `.assay/plugins.toml` belongs to the config story (#62); this crate takes the globs as a parameter.

## Invariants
- Every external call has a timeout and a bounded response size (H-13).
- Every git process is built by `git::command::git`, the only `Command::new("git")` in production code; the only other process is the validated `GroupKiller` program that signals a timed-out process group (`tests/it/single_constructor.rs` enforces both). The constructor clears the environment to what `GitEnvironment` holds, sets `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_ATTR_NOSYSTEM=1`, and passes the §9 `-c` flags including `attr.tree=<empty tree>`; every diff carries `--no-ext-diff --no-textconv`.
- PR objects are fetched only by full SHA over https into a bare repository; refs such as `refs/pull/N/merge` cannot be expressed.
- Every child leads its own process group, has its stdio spooled to anonymous temporary files (no pipes, so no reader can block), and is waited on. A run completes only when the child has exited and its group has no live member; past the deadline, or past the output cap, the whole group is killed and must be verifiably gone (`/proc`), else `ProcessError::Kill`. A group whose leader is reaped is signalled only when `/proc` shows a live member: with none it is left alone, and when `/proc` cannot be read it is `ProcessError::Kill`, never a signal (pgid reuse). A diff past its cap is `needs-split`, never a hang.
- A `/proc` scan skips a process only when it is shown to be gone: its stat read fails with `NotFound` or `ESRCH`, or an unparseable stat line (an exiting process can yield an empty or truncated one; a line without the kernel's trailing newline counts as truncated) is followed by a re-read that does. A process whose stat stays unparseable is an error, never an empty group. Stat files are read as bytes: `comm` is raw and may be any bytes (`prctl(PR_SET_NAME)`, the executable's file name), so only the fields after its last `) ` must be text.
- Every changed path (both sides of renames) is policed; Rust sources, manifests and trust files can never be allowlisted, and allowlisted paths come back as `opaque`, never silently trusted.

## Runtime prerequisites
- Linux `/proc` describing this process (process-group supervision; `/proc/self/stat` must name `std::process::id()`, so a `/proc` from another pid namespace is refused, and must have the shape a `/proc` scan parses, trailing newline included, so such a `/proc` fails once at startup rather than every run) and a procps or util-linux `kill` in `/usr/bin` or `/bin`. Every `GroupKiller` is built after that preflight: `GroupKiller::system` (and so `GitEnvironment::from_process_env`) checks both, and `GroupKiller::at` checks `/proc` before accepting its program, so any `GitEnvironment` carries a checked killer. A failure is `KillerError::ProcUnavailable` (its `ProcFault` says whether the file was unreadable, malformed or named another process) or `KillerError::NotFound` (`docs/architecture/06-operations.md` §5 F22).
- git 2.42 or later (see the last invariant below).
- Spool files live in the system temp directory (`TMPDIR`). Each is bounded by the output cap plus what the group writes before it is stopped: up to one poll interval plus one `/proc` scan until the overshoot is seen, then up to the kill grace (`KILL_GRACE`, 5 s) while the kill runs and the group exits. When the kill fails (`ProcessError::Kill`) the group may still be writing and no bound holds.
- Attribute policing needs git 2.40 or later (`check-attr --source`), and `attr.tree` needs 2.42; an older git fails closed or ignores the setting (then in-tree attributes can only make numstat print `-`, which counts as maximum size).

## Errors
A `thiserror` enum per operation, separating GitHub rejections from transport failures (ERR-10). git: `GitError` (process failure, non-zero exit, unreadable output, missing commit) and `AttributeError` (`Forbidden`, displayed as `gate-error:gitattributes`, or a `GitError`); both fail closed. `ProcessError::Kill` (inside `GitError::Process`) means a process group may still be running, unbounded: callers must abort the job (`gate-error`), not retry or run more git in the same job.

## Test strategy
Integration tests in `tests/it/` against recorded `gh` responses; no live network in tests (TST-10). The git layer is tested against fixture repositories built with plain git in a temporary directory (`tests/it/support.rs`); fetch is covered by its argument list, not by a network fetch. The per-process `/proc` decision is tested with scripted reads, so the exit-mid-scan race is covered deterministically. Run: `cargo test -p assay-forge`.

## Do not
- Do not decide verdicts or severities here; that is the domain's job.
- Do not hold a write token longer than the operation needs.

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); edges are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | The hardened git layer lives here (the charter's "diffs and objects"), not in the domain (I/O). Processes are bounded by polling, not a timeout crate (none is pre-approved). | ARC-02, H-13, H-16 | Story #52 |
| 2026-10-11 | Child stdio goes through anonymous temp files (`tempfile`, pre-approved, now a normal dependency) instead of pipes and drain threads: a grandchild holding a pipe made `thread::scope` joins unbounded, and detaching is forbidden. | H-13 | Story #52 round 3 |
| 2026-10-11 | Round-1 test expectations rewritten during review rounds: `unpoliced_changes_are_not_checked` replaced (every path is now policed), `star_minus_diff_fails_closed` gained `.gitattributes`, `measures_binary_files_as_text` became `a_file_git_will_not_measure_counts_as_maximum_size` (git 2.43 numstat ignores `--text` for NUL content), the single-constructor test admits the kill program, and the grandchild test now covers a group outliving its leader. | H-12, H-16 | coordinator (S1, S2, F1 decisions), Story #52 |
| 2026-10-11 | Allowlisted attribute paths are returned as `opaque` and treated as fully changed downstream, not silently admitted. | T5 | coordinator, Story #52 round 3 |
| 2026-10-11 | Police attributes on every changed path with a base-config glob allowlist that can never cover Rust, manifests or trust files (security pass S1). | T5 | coordinator, Story #52 |
| 2026-10-11 | Kill timed-out process groups through the system `kill` (fixed `PATH`, bounded) rather than `unsafe` or a new `libc` dependency. | H-01, H-16, H-13 | Story #52 |
| 2026-10-11 | A `/proc` that does not describe this process is refused up front (`KillerError::ProcUnavailable`) rather than read as empty groups, and a `ProcessError::Kill` aborts the job (`gate-error`), never a retry. | H-13, ERR-10 | #133 (PR #134) |
| 2026-10-11 | `GroupKiller::at` runs the `/proc` preflight too, so no killer skips it; `ProcUnavailable` carries a `Copy + Eq` `ProcFault` (the `check_proc` test expectations name the fault). A `/proc` scan treats an unparseable stat as an exited process only once a re-read shows it gone, which fixes a flaky `init_accepts_an_empty_directory` without failing open. | OWN-12, ERR-03, H-13 | #135, owner |
| 2026-10-11 | `/proc/<pid>/stat` is read as bytes, so a host process with a non-UTF-8 name no longer fails every run; the `/proc` preflight also requires the stat shape the scan parses (`ProcFault::Malformed`), and the `check_proc` fixtures gained the kernel's trailing newline. | H-10, ERR-03 | #141 |
