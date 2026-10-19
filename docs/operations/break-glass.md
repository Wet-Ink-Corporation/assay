# Break-glass: merging when assay cannot decide

`assay / verdict` is the only required check on a protected base (`docs/architecture/05-security.md` §7). When the pipeline cannot produce a verdict at all, the owner merges through the ruleset bypass. This runbook says when that is allowed, how to do it and how it is recorded. The check lifecycle that leads here is `06-operations.md` §4, step 5.

## Bypass or `/assay override`?

| Situation | What the PR shows | Use |
|---|---|---|
| The workflow never started (Actions outage, workflows disabled, the App uninstalled) | "Expected, waiting for status" on `assay / verdict` | **Ruleset bypass** (this runbook) |
| The verdict check exists but is stuck `in_progress` with no live run | `in_progress` for longer than 2 × the tier p95 | Wait for **reconcile** (every 6 hours) to mark it `failure publish-error`, then `/assay recheck`. Bypass only if reconcile cannot run either |
| assay ran and failed on LLM findings, or on findings the owner accepts | `failure` with reasons | **`/assay override llm`** (or `all`) with the reasons digest from the check summary (`04 §5`) |
| A human-decision gate finding the owner accepts | `failure` naming the finding | **`/assay waive <finding-id>`** |
| A quota or infra deferral | `in_progress` with `assay:llm-pending` | Wait for the requeue, or `/assay override llm` |

The rule: if assay can still publish a verdict, use an assay command, because it is eligibility-checked, recorded in the ledger and shown in the weekly report. The bypass is only for the case where nothing in assay can run. It skips every control assay enforces, gates included.

## Who can bypass

The ruleset bypass list holds **only the break-glass account**, a privileged identity never authenticated in any agent-reachable context (H-06, R3-01). It is never the owner's daily account, which is the agent identity (`docs/decisions/0005`). `assay doctor` checks that the bypass list holds nothing else (`05` T9).

The break-glass account is decided together with the approver account (#41, deferred to go-live, #81). Until then the rulesets are in Evaluate mode (`docs/decisions/0004`), nothing is enforced, and there is nothing to bypass.

## How to bypass

1. Confirm the case. Open the PR's checks: `assay / verdict` must be "Expected, waiting for status" (or stuck with reconcile also down). Check the GitHub status page for an Actions incident.
2. Open the record **before** merging: an issue in the assay repository titled `break-glass: <repo>#<pr>`, labelled `break-glass`, with:
   - the PR link and the head SHA you are merging;
   - why assay could not decide (incident link, error);
   - the gates you ran by hand instead (at least `temper gate` on the head), with their result;
   - why the merge could not wait.
3. Sign in as the break-glass account, in a browser profile that no agent session uses, and merge the PR with "Merge without waiting for requirements to be met (bypass rules)".
4. Sign out of the break-glass account.
5. Add the merge commit SHA to the issue and leave it open for the weekly review.

## Afterwards

- When Actions is back, push nothing to the merged branch. Post `/assay recheck` on the merged PR if its run can still be dispatched, so the head gets a verdict on record; a `failure` there is a finding to fix on `main` in a new PR.
- GitHub records the bypass in the ruleset's rule insights (Settings, Rules, Insights, filter "Bypassed"). That is the source of truth: an issue missing for a bypass shown there is itself an incident.

## Weekly review

The bypass is recorded outside assay, so the ledger has no record of it. Review it in the weekly report:

1. Compare the rule-insights "Bypassed" entries for the week with the open `break-glass` issues. Every bypass needs an issue, and every issue needs a bypass.
2. Note each one in the weekly report issue, under overrides, as `break-glass` with its PR and reason.
3. Close each `break-glass` issue once reviewed and its recheck, if any, is resolved.

Until `assay report` (#113) lists `break-glass` issues opened in its window, do steps 1 and 2 by hand.
