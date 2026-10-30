# 0002 · Owner answers to H-01..H-14

- **Status:** accepted (partly open; see below); H-11 answered 2026-10-30
- **Date:** 2026-10-11
- **Issue:** #36
- **Spec:** `docs/architecture/` v1.2, `07 §3` (human decisions and upstream requests)

`0001` is reserved for the genesis ADR (#38).

## Context

`07 §3` lists the human decisions H-01..H-14 that the spec needs from the owner, each with a default that applies if the owner does not answer. Review round 3 (`docs/review/03-adversarial-review-round-3.md`, R3-01) also proposed changes to H-06, H-08 and H-13.

## Decision

On 2026-10-11 the owner accepted the recommended defaults below and the R3-01 change.

| ID | Answer |
|---|---|
| H-01 | GitHub client: the `gh` CLI behind the `Forge` port (not `octocrab`) |
| H-03 | Adoption order: assay itself, then weigh-in, then temper |
| H-04 | `on_infra_failure = "warn"`, with `block_on_infra_for = ["trust-path", "exec-surface"]` |
| H-05 | Private, internal, `publish = false` |
| H-06 | As changed by R3-01: only accounts never authenticated in any agent-reachable context are privileged. `approvers` holds the dedicated approver account; `owners` holds only privileged accounts (an optional break-glass account, the only ruleset bypass actor); the owner's daily account, connected to Claude Code, is in `maintainers` and takes no privileged action |
| H-07 | GitHub-hosted runners with an egress block; a container or self-hosted runner only if `A-12` fails |
| H-10 | Tooling delivered by GHCR digest |
| R3-01 | Accepted. The approver account (H-13) is now **mandatory for M1**, and H-08 is restated in two parts: (a) agents push under agent ids; (b) no agent-reachable credential can comment, review, label or merge as any id in `owners ∪ approvers` |

## H-11, answered 2026-10-30

The owner tried Greptile and CodeRabbit and found both too expensive. At list prices on 2026-10-30, and at assay's own pace from 2026-10-11 to 2026-10-30 (99 PRs and 373 pushed commits, one human PR author, agents pushing several times an hour), one repository costs about:

- **Greptile Pro:** $1 per base review ($3 Plus), no seat fee. One review per push comes to about $500/month (about $1,500 on Plus).
- **CodeRabbit Essentials:** $30 per PR author per month ($24 annual), with 5 reviews per developer per hour. Further reviews cost $0.25 per reviewed file, so overage dominates at agent push rates (hundreds of dollars a month).

Decision: SaaS spend is high, so Path A continues and **Path B stays the pre-agreed fallback (ADR-18), not a first step.** The repository became public on 2026-10-30, which makes GitHub-hosted Actions minutes free and lowers Path A's cash cost to roughly the subscription. The same change makes CodeRabbit free for this repository; whether to use it as an interim advisory reviewer is a separate owner choice and not part of this answer. A-19 is answered by these figures.

## Still open

| ID | What is open | Tracked by |
|---|---|---|
| H-02 / D-WI | Upstream filing to Weigh-In (journal hook in the plugin, versioned `weighin ci-check --json`, journal append-only verification). Until delivered, M4 proceeds `experimental` and non-required | upstream filing |
| H-09 | Upstream filings to temper (D-TEMPER-1, D-TEMPER-2). Until delivered, assay implements the interim checks itself (`03 §2.1`) | upstream filing |
| H-08 | Owner setup task: agent identity separation, and the two-part M1 behavioural check | #39–#42 |
| H-12 | Owner setup task: create the `assay-verdict` GitHub App and ruleset | #39–#42 |
| H-13 | Owner setup task: create the dedicated approver account (never pushes, never agent-reachable); required for M1 | #39–#42 |
| H-14 | Owner setup task: create the private `assay-ledger` repo, the App's second installation, and its access ruleset | #39–#42 |

## Consequences

- The spec's `07 §3` records the H-06/H-08/H-13 change (R3-01 record row) and ADR-24.
- M1 cannot start until H-08, H-12, H-13 and H-14 are done (`06 §8`, M0 exit criterion).
