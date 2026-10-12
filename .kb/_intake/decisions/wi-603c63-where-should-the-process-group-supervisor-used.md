---
id: "kb-decision-wi-603c63"
title: "Where should the process-group supervisor used by exec gates (#62) and the model runner (#82) live, given gates and runner may not depend on forge: A"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"Where should the process-group supervisor used by exec gates (#62) and the model runner (#82) live, given gates and runner may not depend on forge?\", facing the question an agent raised, we decided for A."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-10-12"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-603c63"
question: "Where should the process-group supervisor used by exec gates (#62) and the model runner (#82) live, given gates and runner may not depend on forge?"
door: two-way
blast_radius: system
decider: "human:ryan"
provenance: user-directed
decided_at: "2026-10-12T00:53:23Z"
tree_hash: "b87e573881f21f6dcbcf82a3f873de65e8ee59e4"
---

# Where should the process-group supervisor used by exec gates (#62) and the model runner (#82) live, given gates and runner may not depend on forge: A

## Context and problem statement

Where should the process-group supervisor used by exec gates (#62) and the model runner (#82) live, given gates and runner may not depend on forge?

Raised by an agent (sweep) as a question and captured by Weigh-In as `wi-603c63`. Anchor: `crates/assay-forge/src/process.rs`.

## Decision outcome

Chosen option: **A**.

Decider's note: Forge public (Ryan, decision card 2026-10-12)

## Provenance

- Decided 2026-10-12T00:53:23Z by human:ryan (user-directed), via cli.
- Raised in session `0af9bc42-376c-52dd-8e5c-98e787854a2c`.
- Verbatim: "Where should the process-group supervisor used by exec gates (#62) and the model runner (#82) live, given gates and runner may not depend on forge?"
