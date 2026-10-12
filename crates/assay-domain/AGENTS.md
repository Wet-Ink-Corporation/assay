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
- `normalise`: the one shared normaliser of 03 §3 (trailing `//` and ` # ` comments dropped, whitespace collapsed, quote styles unified to `"`, `...`/`…` made one wildcard `…` that absorbs adjacent spaces), for the fingerprint and the evidence check. Raw text only: it is not idempotent. Limits outside Rust: only `"` strings are tracked (a `#` or `//` in a single-quoted string can read as a comment), Python's `//` operator is stripped as a comment, and shell backticks are unified with `"`.
- `Anchor` (`fingerprint`, returning `Option<Fingerprint>`; built by `Finding::anchor`, which carries `expected_behavior` for an omission only), `Fingerprint` (`FromStr`, `Display`, serde as 16 lowercase hex) and `ParseFingerprintError`: the fingerprint of 03 §3. `EventPayload::WaiverApplied` carries a `Fingerprint`.
- `Dismissal` (`judge`) and `DismissalMatch` (`Unrelated`, `Suppressed`, `MoreSevere`): the fuzzy dismissal match of 03 §3 and R3-25.

## Invariants
- No I/O: every function is a pure function of its arguments.
- Fingerprints are computed only in this crate. The digest is SHA-256 over class code, rename-resolved head path, enclosing symbol, the first line of evidence with content (normalised, neither blank nor only wildcards) and, for an omission only, its normalised `expected_behavior`, each prefixed by its byte length as a big-endian `u64`, then the occurrence index as a big-endian `u32`; the fingerprint is its first 8 bytes. Golden tests pin both encodings: changing them orphans every recorded waiver and dismissal.
- Fingerprints fail closed: `Anchor::fingerprint` returns `None` for evidence with no line of content, or an omission whose `expected_behavior` has none, so such a finding can never carry a waiver or dismissal. Callers dispose of it as `evidence-missing`.
- `Finding::anchor` gives every omission a fifth field: a missing `expected_behavior` becomes `""`, so an unvalidated omission never hashes or matches as a defect.
- The fuzzy dismissal match compares the whole normalised evidence (and, for omissions, the normalised `expected_behavior`), by Levenshtein ratio over characters (`1 - distance / longer length`), in integers so 0.8 is exact. Evidence without a line of content, and an omission's `expected_behavior` without content, match nothing; text over 400 characters (the schema cap) is never similar. All three fail safe by publishing.
- Contract types serialise to exactly their schema's wire names and reject unknown fields and versions on deserialisation. They are plain data with `pub` fields: length caps, formats and conditional requirements are the schema's, checked by `assay-app` on read. `Severity` orders `nit < minor < major < blocker`.

## Errors
- `ParseFingerprintError` (`Length`, `Digit`): text that is not 16 lowercase hex digits.
- Further rejections are `thiserror` enums per operation; business outcomes only, never infrastructure (ERR-10).

## Test strategy
Unit tests beside the code. `tests/it/` (dev-dependencies `serde_json` and `proptest`) pins every vocabulary to its schema `enum` and, where 03 lists one, to 03's text, round-trips every valid fixture in `schemas/fixtures/` and refuses the invalid fixtures whose fault is one of shape; `assay-app`'s `tests/it/domain_model.rs` checks that what these types write passes the schemas. Property tests for fingerprint stability and state-machine invariants; the `assay-self` golden corpus replays `decide` (02 §7). Run: `cargo test -p assay-domain`.

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
| 2026-10-11 | Normaliser, fingerprint and fuzzy dismissal match (#49) add `sha2` and `thiserror`, and dev-dependency `proptest`, all pre-approved. Hex is written and parsed by hand (no `hex` here, ARC-02). Callers supply the rename-resolved path, the enclosing symbol (empty at file scope) and the occurrence index; the fuzzy match includes 03's class and severity-ceiling rule. | ARC-02, API-03, ERR-01 | #49 |
| 2026-10-12 | wi-b2ac94 default revised on review: fuzzy match uses whole normalised evidence (the fingerprint stays first-line, 03 §3). Stricter, so it fails safe. | 03 §3, R3-25 | #49 review |
| 2026-10-12 | Omissions fingerprint and fuzzy-match on their normalised `expected_behavior` as well as the signature line, so two omissions in one function stay apart; a fifth length-prefixed field, absent for defects. | 03 §3 | owner (Weigh-In wi-f39f2e, option A) |
| 2026-10-12 | Content-less anchors fail closed: `Anchor::fingerprint` returns `Option` rather than taking a validated constructor, because `Anchor` is a short-lived borrowed view whose only producer is `Finding::anchor` and whose fuzzy match already judges content per call; a constructor would add a type and an error for the same check. | ERR-03, API-03 | #49 re-review |
