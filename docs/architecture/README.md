# assay: architecture specification

**Status:** v1.1, after adversarial review round 1 (62 findings), a regression pass (33 consistency defects) and an adversarial round 2 (14 attacks); see [`../review/`](../review/). Not yet implemented. v1.1's additions (`admit`, central ledger and semaphore, decide-only command set, approver model) have not themselves been reviewed.

assay is a PR review orchestrator for Wet Ink repositories: deterministic gates (temper, Weigh-In) plus a tiered fan-out of read-only Claude Code headless reviewers, adjudicated by a pure decision function into one required check published by a dedicated GitHub App, with persisted run bundles for decision replay and an evaluation harness.

| Doc | Contents |
|---|---|
| [01 Context, goals, requirements](01-context-goals.md) | Honest assurance scope, actors, goals G1–G7, principles P1–P10, requirements R-nn, quality targets QA-nn, scale and cost assumptions |
| [02 Architecture](02-architecture.md) | Job topology (open/plan/admit/gates-static/gates-lint/gates-test/review/decide, decide-only), trust zones Z0–Z4, components, input provenance, keys and state, local mode, repo layout |
| [03 Contracts](03-contracts.md) | Base config, gate contract, the verified temper adapter, `assay.findings/1`, run, decision, events, `ModelRunner`, the hardened headless profile |
| [04 Pipeline](04-pipeline.md) | Stages, tiering and trust paths, reviewer roster and budgets, validation order, rubric v1, publish/dismissal, re-review, eval, Weigh-In checkpoints |
| [05 Security](05-security.md) | Threat model T1–T20, permission matrix, secrets, prerequisites enforced by `assay doctor` |
| [06 Operations](06-operations.md) | Auth/quota/cost model, central ledger and semaphore, CI cost and latency, check lifecycle, failure modes F1–F19, fail-open/closed, reports, rollout M0–M6, Path B, kill criteria |
| [07 Decisions](07-decisions-assumptions.md) | ADRs (ADR-01..22), assumption register A-01..29, human decisions H-01..14 and upstream requests, build-vs-buy and economics |

## Reading order

01 → 02 → 05 → 04 → 03 → 06 → 07.

## Conventions

- **Numbers are decisions or labelled assumptions.** Each `A-nn` names how it becomes a measurement and what changes if wrong.
- **AEU** = API-equivalent dollars from the CLI's cost estimate; a relative unit, not a quota (`06 §1`).
- **Trust zones**: no job combines PR-code execution, the model secret and a write token (`02 §3`).
- Facts about sibling repos were verified against their source at the commits read; claims not verified are labelled assumptions or listed in `docs/review/01-…` §5 and `docs/review/02-…` §5.
- **Phases** are M0–M6 (milestones); P1–P10 are design principles.
