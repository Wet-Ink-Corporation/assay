---
id: "kb-decision-wi-9fe37e"
title: "How should ClaudeHeadless pass the prompt to claude: as an argv argument per 03 §8 (128 KiB Linux per-argument limit) or on stdin: A"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"How should ClaudeHeadless pass the prompt to claude: as an argv argument per 03 §8 (128 KiB Linux per-argument limit) or on stdin?\", facing the question an agent raised, we decided for A."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-10-11"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-9fe37e"
question: "How should ClaudeHeadless pass the prompt to claude: as an argv argument per 03 §8 (128 KiB Linux per-argument limit) or on stdin?"
door: two-way
blast_radius: system
decider: "human:ryan"
provenance: user-directed
decided_at: "2026-10-11T23:24:03Z"
tree_hash: "b87e573881f21f6dcbcf82a3f873de65e8ee59e4"
---

# How should ClaudeHeadless pass the prompt to claude: as an argv argument per 03 §8 (128 KiB Linux per-argument limit) or on stdin: A

## Context and problem statement

How should ClaudeHeadless pass the prompt to claude: as an argv argument per 03 §8 (128 KiB Linux per-argument limit) or on stdin?

Raised by an agent (sweep) as a question and captured by Weigh-In as `wi-9fe37e`. Anchor: `crates/assay-runner/src/headless.rs`.

## Decision outcome

Chosen option: **A**.

Decider's note: Stdin (Ryan, decision card 2026-10-11)

## Provenance

- Decided 2026-10-11T23:24:03Z by human:ryan (user-directed), via cli.
- Raised in session `0af9bc42-376c-52dd-8e5c-98e787854a2c`.
- Verbatim: "How should ClaudeHeadless pass the prompt to claude: as an argv argument per 03 §8 (128 KiB Linux per-argument limit) or on stdin?"
