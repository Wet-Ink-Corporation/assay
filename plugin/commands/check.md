---
description: Run assay's advisory check of the working tree, Weigh-In checkpoint (CP1) included
argument-hint: "[--base <branch>]"
allowed-tools: Bash(assay check --local:*), Bash(assay pre-push), Bash(assay status --local:*)
---

Run `assay check --local $ARGUMENTS` from the repository root and report what it printed.

`assay check --local` snapshots the working tree, runs its `exec` gates, has the logged-in
`claude` review the snapshot and decides, as CI would (`docs/architecture/02-architecture.md`
§6). It then lists the current branch's open Weigh-In items (CP1, `04-pipeline.md` §9):
*blocking* items (triaged, and one-way or unsure, the owner's to settle, or naming what they
block) fail the check; items *pending triage* only warn. The result is advisory: CI never reads
it.

When you report:

- Give the decision and its reasons, the gates and the review findings as printed.
- List each blocking Weigh-In item by id and say it must be settled with `/weigh-in` before the
  push; list pending items and suggest triaging them. Do not decide, triage or dismiss an item
  yourself.
- Exit 2 means the check could not run: show the `assay:` line it printed.
- If it says `weigh-in: not checked`, say why (no `weighin` found: set `ASSAY_WEIGHIN` to its
  absolute path; or a detached `HEAD`).

For the Weigh-In items alone, without the snapshot or the model, run `assay pre-push`: it is
what `plugin/hooks/pre-push` runs before a push.
