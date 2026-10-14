---
id: "kb-decision-wi-93b515"
title: "Change in place until v0.1.0"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"Should #170 change the unreleased assay.event/1 schema in place rather than bump to assay.event/2?\", facing #262 changed assay.event/1 in place instead of publishing assay.event/2, we decided for Change in place until v0.1.0 and neglected Bump on every contract change, on the premise that nothing outside the repo reads these events yet, accepting that if wrong: an external reader breaks silently."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-10-14"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-93b515"
question: "Should #170 change the unreleased assay.event/1 schema in place rather than bump to assay.event/2?"
door: two-way
blast_radius: module
decider: "human:ryan"
provenance: user-approved
decided_at: "2026-10-14T16:45:09Z"
tree_hash: "0348842e459a9d4a2d4329b54cbd25a581583aae"
recommended: "A"
flip_condition: "anything outside the repo already consumes assay.event/1"
---

# Change in place until v0.1.0

## Context and problem statement

Should #170 change the unreleased assay.event/1 schema in place rather than bump to assay.event/2?

#262 changed assay.event/1 in place instead of publishing assay.event/2.

Raised by an agent (sweep) as a call and captured by Weigh-In as `wi-93b515`. Anchor: `schemas/event-1.schema.json`.

## Decision drivers

- Churn

## Considered options

### A · Change in place until v0.1.0 (chosen)

Schemas stay at /1 until the first release; changes are noted in the description.

- Holds if nothing outside the repo reads these events yet.
- If wrong: an external reader breaks silently.

### B · Bump on every contract change

Publish assay.event/2 now and bump for each later change.

- Holds if external consumers already exist.
- If wrong: version churn before v0.1.0.

## Evidence

- `schemas/event-1.schema.json`: description notes: unreleased, changed in place by #170

## Decision outcome

Chosen option: **Change in place until v0.1.0**, the recommended option.

Decider's note: Ryan chose A on the decision card 2026-10-14

### Consequences

- Good, because it holds if nothing outside the repo reads these events yet.
- Bad, because if wrong: an external reader breaks silently.

### Confirmation

Revisit when: anything outside the repo already consumes assay.event/1

## Why this might be wrong

an unnoticed external consumer.

## Provenance

- Decided 2026-10-14T16:45:09Z by human:ryan (user-approved), via cli.
- Raised in session `1defee75-55de-52d7-9f81-a8caf064620f`.
- Verbatim: "Should #170 change the unreleased assay.event/1 schema in place rather than bump to assay.event/2?"
