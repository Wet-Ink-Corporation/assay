# 0005 · Agent identity and the approver account (H-08, H-13)

- **Status:** accepted
- **Date:** 2026-10-18
- **Issues:** #42 (decided), #41 (deferred to #81)
- **Spec:** `07` ADR-24, A-17, H-06, H-08, H-13; `04 §5`; `05 §7`

## Context

ADR-24 makes privileged only the accounts never authenticated in any agent-reachable context. Checked on 2026-10-18: every PR an agent opens in this project is authored by the owner's daily account, `Ryan-Britton` (id `124330161`), because the Claude Code environment's GitHub token is that account's. Commits carry `claude` as author metadata, which is not an identity (`04 §2.5`).

## Decision

**#42, agent identity:** the owner's daily account is the agent identity. It is listed in `.assay/agents` and stays in `maintainers`; it is never in `owners` or `approvers`. No machine user is created.

| Account | Id | Lists |
|---|---|---|
| `Ryan-Britton` (daily, connected to Claude Code) | `124330161` | `agents`, `maintainers` |

**#41, approver account:** deferred until M1 go-live (#81). A second account costs a full Enterprise seat for an account that only approves. Before #81 the owner chooses between:
- the spec's dedicated approver account (one seat), or
- an approval signed with a touch-to-sign hardware key, verified against a public key pinned in base `.assay/owners` (no seat; needs a spec change and a story touching #58/#59).

## Consequences

- Every PR from the daily account is agent-authored, so the evidence rule (`04 §5`) applies to all of them, including PRs the owner writes by hand. No rule is looser for a "human" claim (`04 §2.5`), so this costs nothing the spec did not already require.
- Until #41 is resolved, `owners ∪ approvers` is empty: no PR has an eligible approver, and P10 stays a convention. This is acceptable while `assay / verdict` is not required (rulesets in Evaluate, `0004`); #81 cannot go live without it.
- #79 (H-08 behavioural check) checks part (b): no agent-reachable credential acts as an id in `owners ∪ approvers`. Part (a) holds by definition, since the daily account is the agent id.
- The daily account is also an org owner, a privileged role an agent token can reach. The break-glass account (H-06) that would hold org ownership is decided with #41.
- `.assay/owners` and `.assay/agents` are written by #58, which fixes their format, using the ids above.
