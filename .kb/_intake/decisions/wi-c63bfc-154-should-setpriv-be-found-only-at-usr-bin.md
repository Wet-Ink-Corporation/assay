---
id: "kb-decision-wi-c63bfc"
title: "#154: should setpriv be found only at /usr/bin/setpriv or /bin/setpriv (never PATH), and a 126/127 exec failure under setpriv stay charged: as-did"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"#154: should setpriv be found only at /usr/bin/setpriv or /bin/setpriv (never PATH), and a 126/127 exec failure under setpriv stay charged?\", facing the question an agent raised, we decided for as-did."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-11-02"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-c63bfc"
question: "#154: should setpriv be found only at /usr/bin/setpriv or /bin/setpriv (never PATH), and a 126/127 exec failure under setpriv stay charged?"
door: two-way
decider: "human:claude"
provenance: user-directed
decided_at: "2026-11-02T19:12:48Z"
tree_hash: "2f6370800ab5034a3bb710d537a0b84b43a7cb16"
---

# #154: should setpriv be found only at /usr/bin/setpriv or /bin/setpriv (never PATH), and a 126/127 exec failure under setpriv stay charged: as-did

## Context and problem statement

#154: should setpriv be found only at /usr/bin/setpriv or /bin/setpriv (never PATH), and a 126/127 exec failure under setpriv stay charged?

Raised by an agent (sweep) as a call and captured by Weigh-In as `wi-c63bfc`. Anchor: `crates/assay-runner/src/headless/parent_death.rs:1`.

## Decision outcome

Chosen option: **as-did**.

Decider's note: Ryan confirmed the call as made (build session, 2026-11-02, 'Confirm all seven')

## Provenance

- Decided 2026-11-02T19:12:48Z by human:claude (user-directed), via card.
- Raised in session `eae8e9ab-7659-5272-88e7-38dc1a20451f`.
- Verbatim: "kind: call"
