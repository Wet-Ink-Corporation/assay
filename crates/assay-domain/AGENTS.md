# Crate charter: `assay-domain`

## Purpose
The rules of a review: findings, severities, fingerprints, the rubric, tiers, the run state machine and its events, and the `decide` logic over them. Fingerprints are computed here and only here. It is its own crate because it has the narrowest dependency profile in the workspace (no I/O at all) and changes only when review semantics change (ARC-07, `docs/architecture/02-architecture.md` §2).

## Layer
domain. Pure: no I/O, no clock, no randomness, no environment, no processes, no async runtime, no logging subscriber.

## Allowed dependencies
- Workspace crates: none.
- Third-party, and only these (02 §2): `serde` (derives on versioned contract types), `thiserror` (typed rejections, ERR-01), `sha2` (fingerprints). Each is added by the story that first uses it.
- Dev-dependencies: crates on the pre-approved list in `.claude/temper.json`. The architecture test does not check them (it exempts every third-party dev-dependency of the domain); temper's H-16 pre-approval hook governs them, and any other is a human decision.
- Never: any other normal or build dependency, any `assay-*` crate (dev-dependencies included), `weighin-core` or temper code (P8). `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
Workspace-internal, not published (`publish = false`). Re-exported from `lib.rs`:
- Closed vocabularies of 03, each with `ALL` and its wire `code()`, generated from one list by the private `closed_enum!` macro so serde, `code()` and `ALL` cannot drift: `Disposition` and `ReasonCode` (§5); `Class`, `FindingKind`, `Severity`, `IntroducedBy`, `Side`, `Confidence`, `SourceKind`, `GateConclusion`, `FindingsSchema` (§3); `EventKind`, `EventSchema`, `RateLimitStatus`, `VerifierVerdict`, `FeedbackSignal`, `VoidReason` (§6).
- `Marker` (`Marker::of`, `Finding::marker`): the display mapping of 03 §3, blocker 🔴, major 🟡, minor and nit 🟡 (nit), pre-existing 🟣.
- `FindingsDocument`, `Source`, `Stats`, `Finding`, `Caller`: `assay.findings/1` (03 §3).
- `Event`, `EventPayload` (with `GateRun`, `Preflight`, `SpecialistRun`) and `RunUid`: `assay.event/1` (03 §6).
- `Aeu`: an amount of API-equivalent dollars held exactly in micro-AEU (`from_micros`, `micros`, `saturating_add`), displayed as a plain decimal (`0.74`), the form `--max-budget-usd` reads.

## Invariants
- No I/O: every function is a pure function of its arguments.
- Fingerprints are computed only in this crate.
- Contract types serialise to exactly their schema's wire names and reject unknown fields and versions on deserialisation. They are plain data with `pub` fields: length caps, formats and conditional requirements are the schema's, checked by `assay-app` on read. `Severity` orders `nit < minor < major < blocker`.

## Errors
None yet. Rejections will be `thiserror` enums per operation; business outcomes only, never infrastructure (ERR-10).

## Test strategy
Unit tests beside the code. `tests/it/` (dev-dependency `serde_json`) pins every vocabulary to its schema `enum` and, where 03 lists one, to 03's text, round-trips every valid fixture in `schemas/fixtures/` and refuses the invalid fixtures whose fault is one of shape; `assay-app`'s `tests/it/domain_model.rs` checks that what these types write passes the schemas. Property tests for fingerprint stability and state-machine invariants; the `assay-self` golden corpus replays `decide` (02 §7). Run: `cargo test -p assay-domain`.

## Do not
- Do not read time, ids, the environment or files here; the application passes them in through its ports.
- Do not add a dependency outside serde, thiserror and sha2: that changes 02 §2 and is a human decision (H-16).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); dependencies are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | `Disposition` added (Story #47) so schema rejections in `assay-app` map to a domain value; no dependency needed. | API-02 | Story #47 |
| 2026-10-11 | Findings model, vocabularies and event envelope (Story #48) add `serde` (derive) and dev-dependency `serde_json`, both pre-approved. 03 v1.2 wins over the issue: the envelope carries `run_uid` and there are 23 reason codes. Kinds whose `data` 03 lists no fields for are empty struct variants that reject any field (no `serde_json::Value` here), stricter than the schema's open object; they gain fields with the story that defines them. AEU amounts are `f64`, so events are `PartialEq` only. | OUT-02, API-04, ARC-02 | Story #48 |
| 2026-10-11 | `Aeu` added (#82) as integer micro-AEU rather than a float or a decimal crate (none is allowed here), so caps and costs compare and sum exactly. | API-02, H-11 | #82 |
