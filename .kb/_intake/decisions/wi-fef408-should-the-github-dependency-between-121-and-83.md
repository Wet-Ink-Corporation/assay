---
id: "kb-decision-wi-fef408"
title: "Flip: #83 blocked by #121"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"Should the GitHub dependency between #121 and #83 make #83 blocked by #121, instead of #121 blocked by #83 as now?\", facing #121's body says it is blocked by #83, but the AFK run plan says #121 must precede #83, we decided for Flip: #83 blocked by #121 and neglected Keep: #121 blocked by #83, on the premise that #121 can be built without #83's preflight code, accepting that if wrong: #121 lands with nothing to amend and gets reworked with #83."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-10-11"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-fef408"
question: "Should the GitHub dependency between #121 and #83 make #83 blocked by #121, instead of #121 blocked by #83 as now?"
door: two-way
blast_radius: external
decider: "human:ryan"
provenance: user-approved
decided_at: "2026-10-11T16:12:57Z"
tree_hash: "79be0155196b767ce99cc4e9907c8217b70b6c42"
recommended: "A"
flip_condition: "#121 genuinely needs #83's preflight code to exist"
---

# Flip: #83 blocked by #121

## Context and problem statement

Should the GitHub dependency between #121 and #83 make #83 blocked by #121, instead of #121 blocked by #83 as now?

#121's body says it is blocked by #83, but the AFK run plan says #121 must precede #83.

Raised by an agent (sweep) as a call and captured by Weigh-In as `wi-fef408`. Anchor: `https://github.com/Wet-Ink-Corporation/assay/issues/121`.

## Decision drivers

- Critical-path safety
- Build order

## Considered options

### A · Flip: #83 blocked by #121 (chosen)

Remove #121<-#83, add #83<-#121, update #121's body

- Holds if #121 can be built without #83's preflight code.
- If wrong: #121 lands with nothing to amend and gets reworked with #83.

### B · Keep: #121 blocked by #83

Leave as linked

- Holds if #83 and #121 ship back to back.
- If wrong: #83 merges with a clean-init rule that fails every review until #121 lands.

## Evidence

- `issue #121 'Blocked by'`: #83 Preflight ... (this story amends its clean-init rule; implement together or right after)
- `memory afk-run-plan`: new M2 issue #121 ... must precede #83 preflight
- `issue #121 body`: On the pinned CLI that rule fails every run.

## Decision outcome

Chosen option: **Flip: #83 blocked by #121**, the recommended option.

Decider's note: Ryan tapped Flip on the decision card; #121 now blocked by #82 and blocks #83 (#84 link dropped to avoid a cycle)

### Consequences

- Good, because it holds if #121 can be built without #83's preflight code.
- Bad, because if wrong: #121 lands with nothing to amend and gets reworked with #83.

### Confirmation

Revisit when: #121 genuinely needs #83's preflight code to exist

## Why this might be wrong

#121 is written as an amendment to #83's code, so building it first may mean stubbing

## Provenance

- Decided 2026-10-11T16:12:57Z by human:ryan (user-approved), via cli.
- Raised in session `74a5d3c7-8e06-5f1e-abec-ab50a4c56f5d`.
- Verbatim: "Should the GitHub dependency between #121 and #83 make #83 blocked by #121, instead of #121 blocked by #83 as now?"
