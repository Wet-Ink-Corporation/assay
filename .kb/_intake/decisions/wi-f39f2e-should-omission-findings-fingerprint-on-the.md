---
id: "kb-decision-wi-f39f2e"
title: "Should omission findings fingerprint on the normalised expected_behavior as well as the enclosing function signature, so two different omissions in one function don't share a fingerprint: A"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"Should omission findings fingerprint on the normalised expected_behavior as well as the enclosing function signature, so two different omissions in one function don't share a fingerprint?\", facing the question an agent raised, we decided for A."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-10-12"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-f39f2e"
question: "Should omission findings fingerprint on the normalised expected_behavior as well as the enclosing function signature, so two different omissions in one function don't share a fingerprint?"
door: two-way
blast_radius: system
decider: "human:ryan"
provenance: user-directed
decided_at: "2026-10-12T01:11:26Z"
tree_hash: "b87e573881f21f6dcbcf82a3f873de65e8ee59e4"
---

# Should omission findings fingerprint on the normalised expected_behavior as well as the enclosing function signature, so two different omissions in one function don't share a fingerprint: A

## Context and problem statement

Should omission findings fingerprint on the normalised expected_behavior as well as the enclosing function signature, so two different omissions in one function don't share a fingerprint?

Raised by an agent (sweep) as a question and captured by Weigh-In as `wi-f39f2e`. Anchor: `crates/assay-domain/src/finding.rs:46`.

## Decision outcome

Chosen option: **A**.

Decider's note: Add behaviour (Ryan, decision card 2026-10-12)

## Provenance

- Decided 2026-10-12T01:11:26Z by human:ryan (user-directed), via cli.
- Raised in session `0af9bc42-376c-52dd-8e5c-98e787854a2c`.
- Verbatim: "Should omission findings fingerprint on the normalised expected_behavior as well as the enclosing function signature, so two different omissions in one function don't share a fingerprint?"
