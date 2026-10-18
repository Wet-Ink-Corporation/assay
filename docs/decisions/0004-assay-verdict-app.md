# 0004 · The `assay-verdict` App and rulesets (H-12)

- **Status:** accepted
- **Date:** 2026-10-18
- **Issue:** #39
- **Spec:** `docs/architecture/07-decisions-assumptions.md` ADR-11 and H-12, `05-security.md` §3

## Context

ADR-11: only a dedicated App may publish the required check, and the ruleset pins that App's `integration_id`. Otherwise a workflow added in a PR, running as the shared Actions app, could forge the verdict.

## Decision

The owner created the App and rulesets on 2026-10-18.

| Item | Value |
|---|---|
| App | `assay-verdict`, owned by the `Wet-Ink-Corporation` org, installable only on this account |
| App id (`integration_id`) | `5371952` |
| Installation on `assay` | `172801043` |
| Repository permissions | Actions: read and write (requeue dispatch); Checks: read and write; Contents: read; Pull requests: read and write; Metadata: read. Nothing else. Webhook off. |
| Authentication | Private key only (no client secret is used) |
| Key storage | Environment `assay-publish`, deployment branches `main` only. Secret `ASSAY_VERDICT_PRIVATE_KEY`, variable `ASSAY_VERDICT_APP_ID`. No other copy is kept; the key is never agent-reachable (H-06). |
| Key rotation | Annual; next due 2027-10-18 |
| Branch ruleset `main` (id `25356300`) | Evaluate. Targets the default branch. Requires `assay / verdict` from integration `5371952`, branches up to date; blocks deletion and force-push. |
| Push ruleset `assay-push` (id `25356356`) | Evaluate. Restricts `.github/workflows/**`, `.assay/**`, `.claude/**`, `CODEOWNERS`, `.gitattributes`. |
| Bypass actors | None in either ruleset |

## Consequences

- #53 and #61 read the App id from `vars.ASSAY_VERDICT_APP_ID` and the key from `secrets.ASSAY_VERDICT_PRIVATE_KEY`, in jobs that use environment `assay-publish`.
- Both rulesets stay in Evaluate until #81 (M1 go-live): nothing publishes `assay / verdict` before #54, and the build still edits workflows and `.claude/`.
- The branch ruleset targets the default branch, so it covers `main` once the default branch moves from `spec/architecture`.
- H-06 makes the break-glass account the only bypass actor. Rulesets cannot name one user, so a team holding only that account is added with #41.
- #40 adds `assay-ledger` as the App's second installation (H-14).
