---
id: "kb-decision-wi-a01169"
title: "Narrow exceptions"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"How should deny.toml admit jsonschema's transitive once_cell (via ahash, wasm-bindgen) and the MIT-0 license of borrow-or-share, which block #47?\", facing cargo deny rejects #47's jsonschema dependency tree, and only the owner sets license and ban policy, we decided for Narrow exceptions and neglected Allow MIT-0 globally; Drop jsonschema, on the premise that you accept these two transitive crates, accepting that if wrong: remove two lines and the dep."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-10-11"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-a01169"
question: "How should deny.toml admit jsonschema's transitive once_cell (via ahash, wasm-bindgen) and the MIT-0 license of borrow-or-share, which block #47?"
door: two-way
blast_radius: system
decider: "human:ryan"
provenance: user-approved
decided_at: "2026-10-11T18:47:58Z"
tree_hash: "b87e573881f21f6dcbcf82a3f873de65e8ee59e4"
recommended: "A"
flip_condition: "you'd rather not review each MIT-0 crate"
---

# Narrow exceptions

## Context and problem statement

How should deny.toml admit jsonschema's transitive once_cell (via ahash, wasm-bindgen) and the MIT-0 license of borrow-or-share, which block #47?

cargo deny rejects #47's jsonschema dependency tree, and only the owner sets license and ban policy.

Raised by an agent (sweep) as a question and captured by Weigh-In as `wi-a01169`. Anchor: `deny.toml:30`.

## Decision drivers

- policy surface
- cost

## Considered options

### A · Narrow exceptions (chosen)

Add ahash and wasm-bindgen to once_cell's wrappers; add a license exception for borrow-or-share (MIT-0) only

- Holds if you accept these two transitive crates.
- If wrong: remove two lines and the dep.

### B · Allow MIT-0 globally

Add MIT-0 to the allow list plus the two wrappers

- Holds if MIT-0 is fine anywhere (it is more permissive than MIT).
- If wrong: future MIT-0 crates enter unreviewed.

### C · Drop jsonschema

Hand-write validation for the four schemas

- Holds if you want zero new transitive deps.
- If wrong: several days of work, plus drift from the JSON Schemas.

## Evidence

- `deny.toml:12`: allow = [ "MIT", "Apache-2.0", ... "MPL-2.0" ] (no MIT-0)
- `deny.toml:30`: once_cell banned except via insta, tempfile, tracing-*

## Decision outcome

Chosen option: **Narrow exceptions**, the recommended option.

Decider's note: Ryan chose Narrow exceptions on the decision card

### Consequences

- Good, because it holds if you accept these two transitive crates.
- Bad, because if wrong: remove two lines and the dep.

### Confirmation

Revisit when: you'd rather not review each MIT-0 crate

## Why this might be wrong

if MIT-0 crates keep appearing, B saves churn

## Provenance

- Decided 2026-10-11T18:47:58Z by human:ryan (user-approved), via cli.
- Raised in session `0af9bc42-376c-52dd-8e5c-98e787854a2c`.
- Verbatim: "How should deny.toml admit jsonschema's transitive once_cell (via ahash, wasm-bindgen) and the MIT-0 license of borrow-or-share, which block #47?"
