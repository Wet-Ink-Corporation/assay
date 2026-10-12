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
- `Tier` and `PrClass` (04 §2.3, 03 §1), closed vocabularies.
- `Rubric` (`version`, `majors_block_threshold`, `max_requeues`, `is_micro_role`, `is_waivable`): the validated data of `rubric/v1.toml`, built only by deserialising; the caller parses the TOML.
- `decide(&Rubric, &DecisionInputs) -> Decision`: rubric v1 rules 1 to 9, waive and override (04 §5), pure and total. `DecisionInputs` and its parts (`Policy`, `PullRequest`, `GateReport`, `GateOutcome`, `GateFinding`, `ReviewedFinding`, `WeighIn`, `LlmStage`, `Deferral`, `LlmPolicy`, `InfraPolicy`, `OwnerApproval`, `Authorship`, `Waiver`, `Override`, `OverrideScope`) are a decide-specific projection, not a versioned contract.
- `Decision` with `DecisionSchema`, `Conclusion`, `Reason`, `Counts`, `FindingDisposition`, `Waived`, `DegradedRole`: `assay.decision/1` (03 §5); `Decision::reasons_digest` is the digest an override quotes.

## Invariants
- No I/O: every function is a pure function of its arguments.
- Fingerprints are computed only in this crate.
- `decide` is total and deterministic: equal rubric and inputs give byte-identical JSON. Its conclusion is that of the first rule that fires in the order 1, 2, 2a, 3, 4, 5, 6, 7, 8a, 8b, 8c; its reasons are those of every firing rule, in that order; `pending` comes only from rule 8a. `inputs_digest` is `sha256:<hex>` over a canonical length-prefixed encoding of the rubric and every input field (`src/decide/digest.rs`); the reasons digest is `sha256:<hex>` of the sorted reason codes, one per line, `override` excluded.
- Contract types serialise to exactly their schema's wire names and reject unknown fields and versions on deserialisation. They are plain data with `pub` fields: length caps, formats and conditional requirements are the schema's, checked by `assay-app` on read. `Severity` orders `nit < minor < major < blocker`.

## Errors
- `RubricError` (`EmptyVersion`): a rubric file that would make decisions without a `rubric_version`.
- `decide` cannot fail: every input it is given has a decision.
Rejections are `thiserror` enums per operation; business outcomes only, never infrastructure (ERR-10).

## Test strategy
Unit tests beside the code. `tests/it/` (dev-dependency `serde_json`) pins every vocabulary to its schema `enum` and, where 03 lists one, to 03's text, round-trips every valid fixture in `schemas/fixtures/` and refuses the invalid fixtures whose fault is one of shape; `assay-app`'s `tests/it/domain_model.rs` checks that what these types write passes the schemas. `decide` has one test per rule and per precedence pair (`tests/it/decide*.rs`, dev-dependencies `toml` for `rubric/v1.toml` and `proptest`), property tests for byte-identical output, `pending` only from a deferred stage and read-back, and the golden corpus `corpus/golden/` compared byte for byte (`tests/it/golden.rs`), which the `assay-self` gate will also replay (02 §7). Property tests for fingerprint stability and state-machine invariants come with those stories. Run: `cargo test -p assay-domain`.

## Do not
- Do not read time, ids, the environment or files here; the application passes them in through its ports.
- Do not add a dependency outside serde, thiserror and sha2: that changes 02 §2 and is a human decision (H-16).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); dependencies are added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | `Disposition` added (Story #47) so schema rejections in `assay-app` map to a domain value; no dependency needed. | API-02 | Story #47 |
| 2026-10-11 | Findings model, vocabularies and event envelope (Story #48) add `serde` (derive) and dev-dependency `serde_json`, both pre-approved. 03 v1.2 wins over the issue: the envelope carries `run_uid` and there are 23 reason codes. Kinds whose `data` 03 lists no fields for are empty struct variants that reject any field (no `serde_json::Value` here), stricter than the schema's open object; they gain fields with the story that defines them. AEU amounts are `f64`, so events are `PartialEq` only. | OUT-02, API-04, ARC-02 | Story #48 |
| 2026-10-11 | Rubric v1 and `decide` (#51) add `sha2` (inputs and reasons digests; fingerprints #49 will share it) and `thiserror` (`RubricError`), and dev-dependencies `toml` and `proptest`, all pre-approved. The domain does not parse TOML: `Rubric` derives `Deserialize` with validation. `decide` reads a projection (`DecisionInputs`) the application builds; eligibility (#58), fingerprints (#49) and command-record validity are computed before it, and `fp` stays a `String` until #49's type lands. `inputs_digest` is computed inside `decide` over a hand-written canonical encoding, not passed in, so a decision attests what it read. Spec readings, open to the owner: counts cover reviewer findings plus unwaived gate findings; an override is void when the reason codes differ in any way (an added reason, and also a removed one); waivable findings are named by (gate, rule) in the rubric, with temper rule ids `raised`, `exception`, `gate-config`, `test-inventory`, `manifest-switch` for the adapter to emit; `Tier` includes `needs-split`, which `run-1.schema.json` lacks. | ARC-02, OUT-01, H-16 | #51 |
| 2026-10-11 | `Aeu` added (#82) as integer micro-AEU rather than a float or a decimal crate (none is allowed here), so caps and costs compare and sum exactly. | API-02, H-11 | #82 |
