# assay: architecture specification

**Status:** v1.2, after adversarial review round 1 (62 findings), a regression pass (33 consistency defects), an adversarial round 2 (14 attacks) and an adversarial round 3 on v1.1's additions (28 findings); see [`../review/`](../review/). Not yet implemented. R3-01 (privileged identities) changed owner decisions H-06/H-08/H-13, accepted by the owner on 2026-10-11 (`07 §3`).

assay is a PR review orchestrator for Wet Ink repositories: deterministic gates (temper, Weigh-In) plus a tiered fan-out of read-only Claude Code headless reviewers, adjudicated by a pure decision function into one required check published by a dedicated GitHub App, with persisted run bundles for decision replay and an evaluation harness.

| Doc | Contents |
|---|---|
| [01 Context, goals, requirements](01-context-goals.md) | Honest assurance scope, actors, goals G1–G7, principles P1–P10, requirements R-nn, quality targets QA-nn, scale and cost assumptions |
| [02 Architecture](02-architecture.md) | Job topology (open/plan/admit/gates-static/gates-lint/gates-test/review/release/decide, decide-only), trust zones Z0–Z4, components, input provenance, keys and state, local mode, repo layout |
| [03 Contracts](03-contracts.md) | Base config, gate contract, the verified temper adapter, `assay.findings/1`, run, decision, events, `ModelRunner`, the hardened headless profile |
| [04 Pipeline](04-pipeline.md) | Stages, tiering and trust paths, reviewer roster and budgets, validation order, rubric v1, publish/dismissal, re-review, eval, Weigh-In checkpoints |
| [05 Security](05-security.md) | Threat model T1–T23, permission matrix, secrets, prerequisites enforced by `assay doctor` |
| [06 Operations](06-operations.md) | Auth/quota/cost model, central ledger and semaphore, CI cost and latency, check lifecycle, failure modes F1–F21, fail-open/closed, reports, rollout M0–M6, Path B, kill criteria |
| [07 Decisions](07-decisions-assumptions.md) | ADRs (ADR-01..24), assumption register A-01..33, human decisions H-01..14 and upstream requests, build-vs-buy and economics |

## Reading order

01 → 02 → 05 → 04 → 03 → 06 → 07.

## Conventions

- **Numbers are decisions or labelled assumptions.** Each `A-nn` names how it becomes a measurement and what changes if wrong.
- **AEU** = API-equivalent dollars from the CLI's cost estimate; a relative unit, not a quota (`06 §1`).
- **Trust zones**: no job combines PR-code execution, the model secret and a write token (`02 §3`).
- Facts about sibling repos were verified against their source at the commits read; claims not verified are labelled assumptions or listed in `docs/review/01-…` §5, `docs/review/02-…` §5 and `docs/review/03-…` §3 and §5.
- **Phases** are M0–M6 (milestones); P1–P10 are design principles.

## Changelog

- **v1.2** (2026-10-11): adversarial round 3 on the v1.1 delta (`admit`, central ledger and semaphore, decide-only commands, approver model), issue #35. Authoritative per-PR state (push records, command records, bundle pointers, requeue counters, retry grants, a per-head publish lock) moves into the App-only ledger repo and the check run becomes a projection of it (ADR-23); `base-not-protected` is `failure`, not `neutral`; slots are released by a `release` job after review with compare-and-set; every privileged job runs on attempt 1 only; privileged identities are never agent-reachable and the approver account is mandatory for M1 (ADR-24; H-06/H-08/H-13 changed with the owner's acceptance). Triage and patch locations: [`../review/03-adversarial-review-round-3.md`](../review/03-adversarial-review-round-3.md).
- **v1.1** (2026-10-10): regression pass and adversarial round 2 (`../review/02-regression-pass.md`).
- **v1** (2026-10-10): adversarial round 1 (`../review/01-adversarial-review-round-1.md`).
