# Crate charter: `assay-domain`

## Purpose
The rules of a review: findings, severities, fingerprints, the rubric, tiers, the run state machine and its events, and the `decide` logic over them. Fingerprints are computed here and only here. It is its own crate because it has the narrowest dependency profile in the workspace (no I/O at all) and changes only when review semantics change (ARC-07, `docs/architecture/02-architecture.md` §2).

## Layer
domain. Pure: no I/O, no clock, no randomness, no environment, no processes, no async runtime, no logging subscriber.

## Allowed dependencies
- Workspace crates: none.
- Third-party, and only these (02 §2): `serde` (derives on versioned contract types), `thiserror` (typed rejections, ERR-01), `sha2` (fingerprints). Each is added by the story that first uses it. Dev-dependencies from the pre-approved list in `.claude/temper.json` are allowed.
- Never: any other crate, any `assay-*` crate, `weighin-core` or temper code (P8). `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
None yet. Workspace-internal, not published (`publish = false`).

## Invariants
- No I/O: every function is a pure function of its arguments.
- Fingerprints are computed only in this crate.

## Errors
None yet. Rejections will be `thiserror` enums per operation; business outcomes only, never infrastructure (ERR-10).

## Test strategy
Unit tests beside the code; property tests for fingerprint stability and state-machine invariants; the `assay-self` golden corpus replays `decide` (02 §7). Run: `cargo test -p assay-domain`.

## Do not
- Do not read time, ids, the environment or files here; the application passes them in through its ports.
- Do not add a dependency outside serde, thiserror and sha2: that changes 02 §2 and is a human decision (H-16).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); dependencies are added when first used. | ARC-12, CRF-05 | Story #46 |
