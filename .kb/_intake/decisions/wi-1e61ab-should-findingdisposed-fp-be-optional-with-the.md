---
id: "kb-decision-wi-1e61ab"
title: "Optional fp, required by code"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"Should FindingDisposed.fp be optional, with the schema requiring it only for codes recorded after the finding's location is checked, instead of required for every finding-level event?\", facing findingDisposed events now carry the finding's fingerprint, but only where one exists, so fp is optional and the schema requires it per code, we decided for Optional fp, required by code and neglected Add a {document,index} locator, on the premise that findings refused before anchoring stay fingerprint-less, accepting that if wrong: consumers must handle events with no fp for some codes."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-10-14"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-1e61ab"
question: "Should FindingDisposed.fp be optional, with the schema requiring it only for codes recorded after the finding's location is checked, instead of required for every finding-level event?"
door: two-way
blast_radius: module
decider: "human:ryan"
provenance: user-approved
decided_at: "2026-10-14T12:53:29Z"
tree_hash: "6f10fd75535eac1f360dab212bbff30086cbef91"
recommended: "A"
flip_condition: "a consumer must link every finding-level event to its finding"
---

# Optional fp, required by code

## Context and problem statement

Should FindingDisposed.fp be optional, with the schema requiring it only for codes recorded after the finding's location is checked, instead of required for every finding-level event?

FindingDisposed events now carry the finding's fingerprint, but only where one exists, so fp is optional and the schema requires it per code.

Raised by an agent (marker) as a call and captured by Weigh-In as `wi-1e61ab`. Anchor: `schemas/event-1.schema.json`.

## Decision drivers

- Contract change

## Considered options

### A · Optional fp, required by code (chosen)

As merged in #262.

- Holds if findings refused before anchoring stay fingerprint-less.
- If wrong: consumers must handle events with no fp for some codes.

### B · Add a {document,index} locator

Every finding event names its position; fp required wherever a finding exists.

- Holds if consumers need to tie every event to a finding.
- If wrong: a bigger contract change for little gain.

## Evidence

- `schemas/event-1.schema.json`: fp required by if/then for line-not-in-diff-moved, pre-existing, no-trigger, suppressed-by-rule, dismissed-by-owner, duplicate-of-gate, cap-exceeded with from
- `docs/architecture/03-contracts.md §6`: FindingDisposed{code, fp?, from?, to?}

## Decision outcome

Chosen option: **Optional fp, required by code**, the recommended option.

Decider's note: Ryan chose A on the decision card 2026-10-14

### Consequences

- Good, because it holds if findings refused before anchoring stay fingerprint-less.
- Bad, because if wrong: consumers must handle events with no fp for some codes.

### Confirmation

Revisit when: a consumer must link every finding-level event to its finding

## Why this might be wrong

pre-anchoring drops can't be traced to a finding.

## Provenance

- Decided 2026-10-14T12:53:29Z by human:ryan (user-approved), via cli.
- Raised in session `1defee75-55de-52d7-9f81-a8caf064620f`.
- Verbatim: "kind: call"
