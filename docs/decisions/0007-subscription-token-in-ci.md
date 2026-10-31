# 0007 · The subscription OAuth token in CI is within terms (A-01 verified)

- **Status:** accepted
- **Date:** 2026-10-30
- **Decided by:** owner (option A)
- **Spec:** `06 §1` A-01, `07 §2` ADR-02, `0006`
- **Issue:** #37
- **Re-check:** quarterly (next by 2027-01-31), and whenever the sources below change

## Context

ADR-02 runs model review with `claude -p` in GitHub Actions, authenticated by the owner's subscription OAuth token (`CLAUDE_CODE_OAUTH_TOKEN`), with a `ClaudeApi` runner kept ready behind the `ModelRunner` port. A-01 assumes this is within Anthropic's terms and that headless usage is not moved to a separate credit. Sources read on 2026-10-30:

- Claude Code docs, [GitHub Actions](https://code.claude.com/docs/en/github-actions): `CLAUDE_CODE_OAUTH_TOKEN` is "an OAuth token that authenticates with your Claude subscription, available on Pro, Max, Ultra, Team, and Enterprise plans", generated with `claude setup-token`; "runs use your Claude subscription instead of API billing". For a secret shared across repositories it recommends an API key, "since an OAuth token is tied to the subscription of the person who ran `claude setup-token`".
- Claude Code docs, [authentication](https://code.claude.com/docs/en/authentication): the token is for "CI pipelines and scripts where browser login isn't available" and lasts one year. `ANTHROPIC_API_KEY`, when present, takes precedence in `-p` mode; `--bare` does not read the OAuth token.
- [Consumer Terms](https://www.anthropic.com/legal/consumer-terms) (effective 2025-10-08): §3(7) bars automated access "except when you are accessing our Services via an Anthropic API Key or where we otherwise explicitly permit it"; §2 bars sharing account credentials. The docs above are the explicit permission for this use, though they do not cite §3(7) by name.
- [Usage Policy](https://www.anthropic.com/legal/aup): bars reselling or proxying access, "including services that route requests through consumer subscriptions".
- Support, [Claude Code with Pro or Max](https://support.claude.com/en/articles/11145838-using-claude-code-with-your-pro-or-max-plan): Claude Code usage shares the plan's usage limits. No source names a separate credit or limit for headless or Agent SDK use.

## Decision

A-01 is **verified with conditions**. assay keeps the subscription token (ADR-02) as long as all of these hold:

- The token reviews only the owner's own repositories and the owner's and the owner's agents' PRs; fork PRs never reach it (`0006`).
- It is stored only as the `assay-review` environment secret (#43), never as an organisation secret shared across repositories.
- No `ANTHROPIC_API_KEY` is set where the token is used, so runs are not silently billed at API rates, and the runner does not pass `--bare`.
- The owner re-checks the sources above quarterly and on any change to them.

Rejected: switching to the `ClaudeApi` runner now (removes the ambiguity, costs API rates on every review); waiting for written confirmation from Anthropic support (slow; may still be asked for, and this record stands meanwhile).

## Applied

On 2026-10-30 the owner created the `assay-review` environment (deployment limited to `main`, no required reviewers) holding `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`, with no `ANTHROPIC_API_KEY`. The token expires about **2027-10-30**; renew it before then (#43).

The same day, for #43: `assay-publish` was limited to `main` too; the organisation's and repository's default `GITHUB_TOKEN` became read-only (GitHub has no read-none default, so every workflow declares its own `permissions`), and "Allow GitHub Actions to create and approve pull requests" was turned off.

## Consequences

- `06 §1` A-01 and `07 §2` ADR-02 point here.
- A-01 falls, and ADR-02 switches to the `ClaudeApi` runner, if Anthropic states that subscription tokens may not be used for unattended CI, if headless usage moves to a separate credit, or if assay needs an org-shared secret.
