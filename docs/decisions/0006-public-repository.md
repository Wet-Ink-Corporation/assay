# 0006 · The assay repository is public; fork PRs are untrusted

- **Status:** accepted
- **Date:** 2026-10-30
- **Decided by:** owner
- **Spec:** `01 §7` (fork / external contributor), `07 §3` H-05, ADR-03, `05 §2` T3, T19, `06 §1` A-01
- **Follow-up:** #60 (the pipeline workflow refuses fork PRs before any secret-bearing job)

## Context

The spec scoped v1 to the owner's private repositories: `01 §7` lists fork and external contributors as out of scope ("design must fail safe if one appears"), and H-05 answered "private". On 2026-10-30 the owner made `Wet-Ink-Corporation/assay` public, which makes GitHub-hosted Actions minutes free (`0002`, H-11) and lets anyone open a pull request from a fork.

Checked the same day: every workflow on `main` (`rust.yml`, `release.yml`, `warm-target.yml`) runs on `push`, `pull_request`, `schedule`, `workflow_dispatch` or tags only, with `contents: read` by default. None uses `pull_request_target` or `workflow_run`, so a fork PR runs with a read-only token and no secrets, and the review secrets sit in environments that only `main` can use (`0004`, #43).

## Decision

- The repository stays **public**. Crates stay unpublished (`publish = false`); H-05 now reads "public repository, unpublished crates".
- A fork PR, and any PR whose head repository is not this repository, is **untrusted input that never reaches a secret or the subscription**. The pipeline (`assay.yml`, #60) and decide-only (`assay-decide.yml`, #61) workflows run on `pull_request_target`, so before any job that holds a secret, an environment or the OAuth token they must refuse such a PR (a completed `failure` with its reason, never a skipped check), and a test must pin that guard on every `pull_request_target` workflow. This is the "fail safe" `01 §7` already required, now a requirement on #60 and #61.
- Owner settings applied 2026-10-30: workflow runs from **all external contributors require approval**; secret scanning, push protection and private vulnerability reporting are on.
- A-01 is unchanged in scope: the subscription token reviews only the owner's and the owner's agents' PRs, never a fork's.

## Consequences

- `01 §7`'s fork row and `07 §3`'s H-05 row point here.
- Until #60 lands there is no `pull_request_target` workflow, so nothing changes in CI today.
- Going private again would restore the original assumptions and the Actions charges; this record would then be superseded.
