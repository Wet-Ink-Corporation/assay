# Adversarial review, round 3 (v1.1 delta)

Spec under review: `docs/architecture/` v1.1 at `194c6ac`. Scope: only the structure v1.1 added that no earlier reviewer attacked (`02-regression-pass.md` §5, last bullet), namely:

1. the `admit` job (S1a: budget, slot acquire/wait/defer, reservation, context pack);
2. the central ledger and semaphore (ADR-19: slot refs with lease commits, 45-minute TTL, CAS on the file sha, 24-hour sum);
3. the decide-only command set and workflow (ADR-20: `/assay waive`, `override`, `dismiss`, `retry`, `weigh-in-ack`; default-branch triggers, loop guards, bundle-pointer provenance);
4. the approver model (H-06, H-13, P10 owner eligibility, ADR-16).

Issue: #35. Date: 2026-10-11. Result: **v1.2**, patches applied to `docs/architecture/` (see the Disposition column in §6). The owner accepted R3-01 on 2026-10-11. New register entries: ADR-23, ADR-24; T21–T23; F20–F21; A-30–A-33; M1 canaries in `06 §8`.

## 1. Method

One fresh-context reviewer read all seven architecture documents, the README, both earlier review logs, and the acceptance criteria of the affected Story issues (#55–#59, #61, #70–#73, #100, #101, #103, read via the REST API). Each area was attacked through six lenses: hostile PR author, compromised or prompt-injected agent, races and concurrency, failure and partial-write modes, GitHub API semantics, and cost/quota abuse.

GitHub behaviour was checked against primary sources where it matters. The sources were the `github/docs` repository (raw markdown) and the official REST description (`github/rest-api-description`, `api.github.com.json`). `docs.github.com` itself was not fetched. Every finding carries a confidence tag:

- **VERIFIED**: confirmed against those sources or against the spec's own text (for internal contradictions).
- **OBSERVED**: seen in this review session's own environment.
- **REASONED**: an inference from documented behaviour.
- **UNSURE**: the reviewer could not confirm it. Treat it as a canary to add, not as a fact.

False positives cost the owner time. §5 therefore lists the attacks that were tried and failed, and one premise of my own that verification proved wrong.

## 2. Triage rules

The triage rules are the same as in `01-adversarial-review-round-1.md` §2. The column below is the **recommended** verdict for the owner:

- ✔ accept the finding and the patch;
- ◐ the problem is real, but the patch should be narrowed or staged, or verified first;
- ⏸ defer;
- ✘ reject.

Severity is the reviewer's own:

- **critical**: a merge-gate bypass or a total control failure reachable by an in-scope adversary;
- **high**: a bypass that needs a precondition, a deadlock on the PRs the design most cares about, or a control that the spec asserts but the platform cannot provide;
- **medium**: a material spec gap or abuse path with bounded impact;
- **low**: hygiene or a small inconsistency.

## 3. What I verified before writing

- **`pull_request_target` now runs the default branch's workflow file** (VERIFIED, `events-that-trigger-workflows.md`). On github.com (outside GHES < 3.20), `GITHUB_SHA` is the last commit on the default branch and `GITHUB_REF` is the default branch, whatever the PR's base. A-28's claim for `labeled`/`unlabeled` therefore holds, and is stronger than the spec assumed. P2's wording ("workflow … from the PR's base commit") is no longer literally true for non-default protected bases (R3-27).
- **`workflow_dispatch` can be dispatched against any branch or tag** through the API once the workflow has run at least once (VERIFIED). The dispatched run executes that ref's version of the file.
- **Anyone with write access can edit and delete other users' comments** on issues and pull requests (VERIFIED, `managing-disruptive-comments.md`). The edit history is visible in the UI. The REST `issue-comment` object keeps the original `user` and `created_at`, and exposes `updated_at` and `performed_via_github_app`. The `pull-request-review` object has neither field (VERIFIED, REST schema).
- **People with write permission can re-run a whole run, its failed jobs, or a specific job**, up to 30 days after the initial run and at most 50 times (VERIFIED). Re-runs use the original `GITHUB_SHA`/`GITHUB_REF`, so they execute the original workflow file, and they carry the privileges of the actor who triggered the original run.
- **Concurrency groups hold one running and one pending entry.** A newly queued run cancels and replaces the existing pending one (VERIFIED). On github.com an optional `queue: max` lets up to 100 entries wait, but it cannot be combined with `cancel-in-progress: true`.
- **`PATCH /check-runs/{id}` has no expected-value parameter**, and the REST description contains no `If-Match` support on any endpoint (VERIFIED). `started_at` is settable by PATCH. Annotations "are appended to the list of annotations that already exist for the check run" on every update (VERIFIED).
- **`DELETE /git/refs/{ref}` takes no precondition.** `PATCH /git/refs/{ref}` with `force: false` checks only that the update is a fast-forward, not an expected old value (VERIFIED). `POST /git/refs` fails if the ref exists, so A-26's acquire half holds.
- **`GET /pulls/{n}/commits` returns at most 250 commits** (VERIFIED).
- **`schedule` runs can be delayed under load, especially at the top of the hour**, and "some queued jobs may be dropped" (VERIFIED).
- **`workflow_run` runs the default-branch file**, can hold secrets even when the triggering workflow did not, and chains at most three levels (VERIFIED).
- **This review session itself** (a Claude Code cloud session) has `gh` and GitHub MCP tools (`add_issue_comment`, `pull_request_review_write`, `merge_pull_request`, …) that act under the user's GitHub authorisation. Read access was exercised; no write was attempted (OBSERVED).

## 4. Findings

### Critical

#### R3-01 · The owner's daily account can be reached by agents. H-08 tests push identity, but the privileged channels are comments, reviews and merges.
- **Location:** `01` §2 (Owner, Agent rows), P10; `04` §5 (eligibility bullet); `05` T9 (residual), §7; `07` ADR-16, H-06, H-08, H-13, A-17.
- **Confidence:** VERIFIED that the channels exist; OBSERVED that a cloud session holds them; REASONED for the bypass.
- **Attack:**
  1. A prompt-injected Claude Code session that holds the owner's GitHub authorisation acts as the owner. Every cloud session has it (OBSERVED above), and every local session that inherits the owner's `gh auth` has it too.
  2. The session submits an APPROVE review as the owner on a PR authored and pushed by an agent identity. The owner is not the author, a committer or the recorded push sender, so by `04` §5 the owner is eligible, and rule 4 passes.
  3. The same holds for `/assay override`, `/assay waive`, `/assay dismiss` and `/assay weigh-in-ack` comments.
  4. `05` §7 makes the owner the only ruleset bypass actor. `merge_pull_request` as the owner may therefore skip the required check outright. Whether bypass applies to a user-to-server App token is UNSURE; it does for the user's own token.

  H-08 and the M1 behavioural check only ask whether agents *push* under a non-owner identity. They would pass while every privileged channel stays reachable.
- **Impact:** R-07 (trust-path approval), R-08 (waiver, override), P10, `04` §9 rule 2 and the break-glass all reduce to "the agent chose not to". T9 already admits that shared credentials void the control. The new point is that the spec's verification step cannot detect the shared credential, because it tests the wrong channel.
- **Patch:**
  1. **`01` §2 and P10, `04` §5:** a *privileged identity* is an account never authenticated in any agent-reachable context: no Claude GitHub App authorisation, no `gh` token on a machine where agents run, and no PAT given to a session. `owners ∪ approvers` lists privileged identities only. The owner's daily account, which is connected to Claude Code, moves to a new `maintainers` list: it may author and push, but takes no privileged action.
  2. **H-13 is required for M1 unconditionally.** The approver account, plus an optional separate break-glass account, are the privileged identities.
  3. **Restate H-08 as two parts:** (a) agents push under agent ids; (b) no agent-reachable credential can comment, review, label or merge as any id in `owners ∪ approvers`. M1 behavioural check: from a cloud session and a local session, try to comment and approve as each privileged id in a canary repo. Every attempt must fail.
  4. **Defence in depth:** reject a privileged command whose issue comment has a non-null `performed_via_github_app`. Reviews carry no such field, so this filter cannot cover approvals; record that limitation.
  5. **`05` §7:** the ruleset bypass list holds only the break-glass account, and doctor asserts this.
  6. **T9:** reword the residual to name comment, review and merge, not only push.

#### R3-02 · Any writer can edit an owner's comment into a privileged command
- **Location:** `04` §5 ("a comment-based action must be later than the same clock"); `04` §6 (approval, waiver, override and dismissal are re-evaluated by decide-only); `02` §1 (decide-only "snapshots approvals / waivers / override / dismissals"); `02` §4 (approvals… row); `04` §9 rule 2; `05` T9.
- **Confidence:** VERIFIED for the GitHub permission; REASONED for the chain.
- **Attack:**
  1. An agent identity with write access (agents push branches, so they have it) edits a comment the owner posted after the latest push. The comment can be an earlier `/assay recheck` or any conversational note.
  2. The new text is `/assay override ok`, `/assay waive <fp>`, `/assay dismiss <fp> x` or `/assay weigh-in-ack <digest>` (the digest is printed in the summary).
  3. The comment's `user.id` is still the owner and its `created_at` is still after the push clock.
  4. The agent then posts `/assay recheck`, which any collaborator may trigger (T9). decide-only snapshots the comments, and the privileged action is attributed to the owner.

  Deleting an owner's command is the denial-of-service form of the same attack.
- **Impact:** the agent obtains any privileged outcome without holding a privileged credential, even when R3-01 is fixed. This defeats ADR-16 and ADR-22, and the `04` §9 corroboration.
- **Patch:**
  1. **Commands become events, not state.** decide-only parses a command only from the payload of the `issue_comment.created` event that triggered it, and checks eligibility and the push clock there.
  2. It then appends an App-authored **command record** to the ledger repository: `commands/<repo_id>/<pr>.jsonl` holding `{comment_id, actor_id, created_at, head_sha, body_sha256, command, args}`.
  3. Every later `decide`, both pipeline and decide-only, reads command records and never comment bodies.
  4. At read time it re-fetches the comment. The record is void if the comment was deleted, if `updated_at != created_at`, or if the body hash differs. An edit can therefore only revoke a command, never grant one.
  5. Dismissal reasons and override reasons are taken from the record.
  6. Add the acceptance criterion "editing an owner comment into a command has no effect" to #58, #59, #61 and #100.

#### R3-03 · The per-SHA verdict is shared across bases, and `neutral base-not-protected` plus a retarget gives a merge window
- **Location:** `02` §3 (no `branches:` filter; `decide` publishes `neutral (base-not-protected)`); `03` §5 ("`neutral` … counts as passing for required checks"); `04` §5 rule 2; `06` §6 (`base-not-protected` row); `05` T15; `02` §5 (decide re-fetches the *head* only); R-01.
- **Confidence:** VERIFIED for each step; REASONED for the chain, which is cheap.
- **Attack:**
  1. An agent that can merge builds head H on the current tip of `main`, then opens PR H→`scratch`, an unprotected branch it created.
  2. The default-branch workflow runs (VERIFIED, §3) and the App publishes `neutral base-not-protected` on *the* check run for (H, `assay / verdict`).
  3. The agent retargets the PR to `main` and calls merge at once. GitHub sees a passing `assay / verdict` on H, and the branch is up to date. The new run needs a runner start (about 80 s, `06` §3) before `open` re-stamps the check to `in_progress`.

  Two variants make it worse:
  - **No race:** R3-10's eviction drops the retarget run entirely, and the stale neutral stays.
  - **Second protected base:** with `release/*` protected under a laxer `.assay` (older `owners`, fewer trust globs), the agent gets a genuine `success` there, retargets to `main`, and merges inside the window.

  Separately, `decide` checks only head supersession, so a run planned for base `x` still publishes after the retarget.
- **Impact:** every gate is bypassed, owner approval included, by any identity allowed to merge. No PR code execution is needed.
- **Patch:**
  1. **`base-not-protected` ⇒ `failure`, never `neutral`.** The check is not required on that base, so nothing is blocked there, and a stale result is fail-safe if the head is later targeted at a protected base. Update `03` §5, `04` §5 rule 2, `06` §6 and T15.
  2. **Bind the check to its base:** `assay / verdict` is published only for the default branch. Each other protected base uses `assay / verdict @<base>`, required by that base's own ruleset.
  3. **Re-check the base:** every `decide` and decide-only re-fetches `pull.base.ref` and writes nothing when it differs from `run.base_ref`.
  4. **Optional:** doctor asserts that agent identities cannot merge into protected bases. This needs a push restriction on the base; it is plan-dependent and UNSURE on Team.

### High

#### R3-04 · The environment's deployment-branch policy is the real key control, and it is unspecified and unchecked
- **Location:** `05` §5 ("App key … of base-defined workflows (default-branch file…)"), §4, §7; ADR-20; T20; `06` §1 (requeue dispatches `workflow_dispatch`).
- **Confidence:** VERIFIED (semantics); REASONED (exposure).
- **Attack:** two routes reach the App key.
  - (a) An agent pushes a branch whose new workflow (`on: push`, or `workflow_dispatch` dispatched against that branch, VERIFIED) references `environment: assay-publish`.
  - (b) An agent dispatches `assay.yml` against its own branch, where the file has been edited.

  Either way the job gets the App key unless the environment restricts deployment branches. ADR-20 and T20 reason about *which file a trigger runs*. That keeps the declared triggers safe, but it is not what grants the secret: the environment is. `05` §5 says "(branch policy)" only for the OAuth environment, and doctor (`05` §7) checks neither.
- **Impact:** the App key leaks, which is a total break (T7), in any consumer repo where the environment was created without a policy. The key is installed in every consumer repo, so the weakest repo decides for all of them (see R3-20).
- **Patch:**
  1. **`05` §5:** `assay-publish` and `assay-review` use deployment branches "selected: default branch only". This is correct for every legitimate trigger, because `pull_request_target`, `issue_comment`, `schedule` and `workflow_dispatch`-on-default all have `GITHUB_REF` = default (VERIFIED).
  2. **`05` §7:** doctor asserts both policies in every consumer repo.
  3. **F19 and `02` §5 provenance:** a dispatch run's `head_branch` must equal the default branch.
  4. **A-28:** mark verified (§3).

#### R3-05 · Manual re-runs bypass `admit`, defeat the gate-retry rules, and bring back old workflow and binary versions for 30 days
- **Location:** `06` §1 (caps, semaphore, cooldown); `04` §1 S1a, S2b; `06` §3 (Z2b retried once on infra errors only); `06` §5 F9 ("temper findings are never retried"); T12; `05` §4.
- **Confidence:** VERIFIED for the re-run rights and semantics. REASONED that a re-run of one job reuses the upstream jobs' results from the prior attempt; this is standard behaviour, but confirm it with a canary.
- **Attack:**
  - (a) **Spend without admission.** An agent with write access re-runs only the `review` job of a completed run. `admit` is not re-run, so its outputs (`allowed`, slot) are reused. The LLM spends a full tier with no budget read, no slot and no cooldown, up to 50 times per run. The `decide` that follows deletes a slot ref the run no longer owns (see R3-06(d)).
  - (b) **Flake laundering.** The agent re-runs `gates-test` until a flaky or hostile test passes; `decide` re-runs as its dependent and publishes `success`.
  - (c) **Resurrection.** Re-running a 29-day-old run executes the workflow file and base `.assay` digest *of that run*, with *today's* environment secrets. A security fix to `assay.yml` or to the binary therefore does not reach re-runs for 30 days.
- **Impact:** quota abuse that ignores every guard in `06` §1; gate integrity reduced to "re-run until green"; fix-forward does not work.
- **Patch:**
  1. **Ship the guard from day one.** Every job that holds a secret or a write token (`open`, `admit`, `review`, `decide`, decide-only, `requeue`, `reconcile`) carries `if: github.run_attempt == 1`; for `decide` it is `always() && github.run_attempt == 1`. It must be in the **first shipped template**, so that no version without it ever exists to resurrect.
  2. **Belt and braces:** the binary refuses `GITHUB_RUN_ATTEMPT != 1`.
  3. **Recovery** is `/assay recheck`, which starts a fresh run.
  4. **Doctor** asserts the guard.
  5. **M1 canary:** re-running `review` alone spends nothing and publishes nothing.

#### R3-06 · Semaphore leases leak, are stolen and are held through the gates; a busy slot means no LLM review for most PRs
- **Location:** `06` §1 (Semaphore, Reservations, Budget read); `06` §4 step 4 (reconcile every 6 h); `06` §5 F17 ("Reaped after the 45-minute TTL"); `02` §1 (`decide` box: "ledger write, release slot"); `03` §5 ("a superseded run writes nothing"); `04` §2.4; `01` A-02/A-03; A-26.
- **Confidence:** VERIFIED for (d) and for the spec text; REASONED for the rest.
- **Attacks and failures:**
  - **(a) Leaks on supersession.** A superseded or cancelled-by-newer run "writes nothing": no ledger record and no `SlotReleased`. If `decide` does not run at all, the result is the same. The only other releaser is `reconcile`, every **6 h**, not "after the 45-minute TTL" as F17 states. Agents push while reviews run: A-02 assumes 2.5 runs per PR, so this is the normal path, not an attack. Two such pushes leak both slots, and every `admit` in every repo then waits 10 minutes and defers for up to about 6 h 45 m.
  - **(b) Unmetered spend.** A killed review spent real quota, but SIGTERM loses `total_cost_usd` (`03` §8) and nothing is ledgered. The per-PR cooldown and the 24-hour sum read completed records, so they never count it.
  - **(c) The slot outlives the review.** The slot is held from `admit` to `decide`, so it spans the gates as well. `gates-lint` allows 1,200 + 300 + 300 s and `gates-test` 1,500 + 600 s, doubled by the infra retry. A hostile `build.rs` or test can stall to its step timeouts and keep the slot long after the review has finished. The 45-minute TTL is also shorter than a legitimate worst-case run, so a live lease can "expire".
  - **(d) Stealing.** `DELETE /git/refs` takes no precondition (VERIFIED). Suppose run A's lease expires and is reaped, and run B acquires the slot. A's `decide` then deletes B's ref, and a third admission makes the real concurrency 3. A-26 covered only acquisition.
  - **(e) Unreviewed merges.** A busy slot surfaces as `budget-deferred`. For PRs that are not trust-path, that means `success` with `llm-degraded` (`04` §2.4, `06` §1). While both slots are held, legitimately or through (a) or (c), every such PR merges without LLM review, and an agent can manufacture that state.
- **Patch:**
  1. **Release after review, not after decide.** Add a Z4 job `release` (`needs: [admit, review]`, `if: always() && needs.admit.outputs.slot != '' && github.run_attempt == 1`). It releases the slot and writes the ledger record with the real cost, or the cap when the review died. `decide` stops releasing.
  2. **Release and reap by compare-and-set only:** `git push --force-with-lease=refs/heads/slot-N:<lease_commit> <ledger> :refs/heads/slot-N`; receive-pack checks the old value atomically. Never use REST DELETE.
  3. **"Writes nothing" covers only the check and comments** for superseded or cancelled runs; the ledger record and the release always happen.
  4. **`admit` reaps expired leases itself**, by compare-and-set, before it waits; `reconcile` is a backstop. Correct F17.
  5. **Size the TTL to the review.** Set review `timeout-minutes` ≤ 40 with a 45-minute TTL, since the slot now spans only the review.
  6. **Count admissions, not completions.** `admit` writes a `reserved` record when it acquires the slot, and the cooldown and the 24-hour sum count those.
  7. **A busy slot is its own reason, `slot-busy`.** For agent-authored PRs, it never yields an `llm-degraded` success: the check stays `in_progress` and requeues. Report its frequency.

#### R3-07 · The push record and push clock are mis-specified: "Update branch" disqualifies the owner, non-push events overwrite the pusher, and the clock moves
- **Location:** `04` §5 (rule (c), the clean-merge exclusion, the push clock); `02` §4 (pusher set row); `02` §1 (`open` box); `04` §1 S0; `06` §4 step 1; T9; ADR-16. Acceptance criteria of #58.
- **Confidence:** VERIFIED for (a) as an internal contradiction and for `started_at` being settable; REASONED for the rest.
- **Problems:**
  - **(a) Contradiction.** Clean merge-from-base commits are excluded from (b) only. Pressing "Update branch" is a push whose `synchronize` sender is the owner, so rule (c) disqualifies the owner anyway. The stated purpose ("does not disqualify their own approval") and #58's acceptance criterion are both false under the rule as written. Because "require branches up to date" is mandatory (`05` §7), this is the usual path for every PR that waits for approval.
  - **(b) Overwrite.** `open` "records push sender" on every run, yet runs on the same head also come from non-push events: `reopened`, `ready_for_review`, `edited` with a base change, a requeue `workflow_dispatch`, and re-runs. Recording `sender` on those events has two effects:
    - The owner who clicks "Ready for review" becomes a "pusher".
    - The real pusher is overwritten. An agent that pushes through the owner's authorisation (sender = owner) closes and reopens the PR as its agent identity. The record now names the agent, and the owner is eligible again: exactly the case (c) exists to catch.
  - **(c) Scope.** The record lives in the check run for one SHA. "A push to this PR" therefore means the current head only, and earlier heads' checks cannot be listed (ADR-19 says so).
  - **(d) Clock.** If `open` re-stamps `started_at` on a same-head re-run, it silently voids every valid approval and command. That deadlocks deferred trust-path PRs requeued by dispatch. The spec does not say whether `open` re-stamps it.

    There is a smaller side effect too. An approval submitted after the push but before `open` runs (about 80 s) is ignored forever, without a message.
  - **(e) Unknown pushers.** For `opened`, pushes made before the PR existed have no recorded sender. Commits whose emails map to no GitHub user leave both (b) and (c) empty.
- **Patch:**
  1. **Keep push records append-only per PR in the ledger:** `pushes/<repo_id>/<pr>.jsonl` holding `{before, after, sender_id, forced, at}`, where `at` is the triggering workflow run's `created_at`. `open` writes them for `synchronize` only. The `opened` sender is recorded as `opener`, not as a pusher.
  2. **Rule (c)** is the union of `sender_id` over all of the PR's records, except records whose `after` is a clean merge-from-base commit. Z4 decides cleanliness: two parents, `parent1 == before`, `parent2` an ancestor of the base, and a tree equal to a conflict-free `git merge-tree --write-tree`.
  3. **The push clock** is the `at` of the latest record whose `after == head_sha`. `open` never writes `started_at` on an existing run.
  4. **Unattributed commits.** A commit in the range whose author or committer is not a GitHub user (apart from `web-flow` as committer) counts as authored by every owner, so the approver is required.
  5. **Document the update modes:** "Update branch (merge)" keeps the owner eligible; "Update with rebase" rewrites committers and does not.
  6. **Say when an approval is ignored:** the summary shows "approval at T ignored: before push at T′".

#### R3-08 · Bundle provenance rejects requeued runs, and decide-only's own decisions have no persisted bundle
- **Location:** `02` §5 (check output pointer: run "event `pull_request_target`"); `05` T11; `06` §5 F19; ADR-20; `06` §1 (requeue runs the pipeline via `workflow_dispatch`); `02` §1 (decide-only "CAS-updates the check"); `02` §4 (context pack "dismissals recorded by decide"); QA-11.
- **Confidence:** VERIFIED (spec text).
- **Failures:**
  1. The requeue path runs the whole pipeline under `workflow_dispatch`, so its `decide` writes a pointer to a run whose event is `workflow_dispatch`.
  2. decide-only accepts only `pull_request_target`. The owner's `/assay recheck` after approving a requeued PR therefore ends in `failure gate-error:bundle-digest` (F19), and only a new push recovers.

  Only trust-path and exec-surface PRs defer to `in_progress` and get requeued, so this deadlock lands exactly on the PRs that need approval.

  decide-only's own decisions are a second gap. The approval flip, waivers and overrides are re-decided in decide-only, and the spec persists nothing for them. That leaves two bad options:
  - It overwrites the pointer with its own run. The next decide-only then rejects the pointer, because the event is `issue_comment`.
  - It does not persist at all. Then QA-11 ("100% of conclusions reproducible") fails for exactly the conclusions that matter, and `admit`'s "dismissals recorded by decide" have no home.
- **Patch:**
  1. **Split the pointer in two:**
     - `evidence: {run_id, run_attempt, sha256}`. Allowed provenance: workflow `assay.yml`; event ∈ {`pull_request_target`, `workflow_dispatch`}; `head_branch` = default; `run_attempt == 1`.
     - `decision: {run_id, sha256}`. Allowed provenance: workflow `assay-decide.yml`; events {`issue_comment`, `pull_request_target`, `schedule`}.
  2. **decide-only uploads a small decision bundle**: the snapshot, the decision and the evidence pointer. Replay follows the chain.
  3. **Command and dismissal records live in the ledger** (R3-02), and `admit` reads them there.

#### R3-09 · "Compare-and-set" on the check run is not atomic on GitHub
- **Location:** `02` §5 (two-phase `external_id`, "compare-and-set on `check_run.id` and the previously read `external_id`"; "the pipeline's `decide` has no shared group (CAS arbitrates)"); `04` §1 S8; `06` §4 step 3; T10; R-01; the round-2 resolution of R2-5.
- **Confidence:** VERIFIED that there is no precondition; REASONED for the interleavings.
- **Attack and race:** the update endpoint takes no expected value, so the design's "CAS" is read-then-write. Two interleavings show the problem:
  - **(i) A lost re-decision.** decide-only, flipping an approval to `success` from bundle B1, reads stamp K1. A requeue or retarget run's `open` then stamps `pending:R2`, and decide-only writes `success` over it. When R2's `decide` runs, it finds a non-pending stamp. The "stale run yields" rule can make it yield, so the requeue or retarget result is lost and `success` stands.
  - **(ii) Two pipeline decides for one head**, in the gap before cancelled-by-newer is detected.
- **Impact:** a stale or wrong final conclusion, which combines with R3-03. The R2-5 fix rests on a primitive that does not exist.
- **Patch:**
  1. **Lock each head:** a per-(repo, head SHA) publish mutex, as an atomically created ref `refs/heads/lock-<repo_id>-<head_sha>` in the ledger. It uses the same primitive as the slots, with a 2-minute TTL and compare-and-set release.
  2. **Hold it across read → decide → write.** The stamp becomes `pending:<run_id>:<seq>`, with `seq` taken from the ledger push record, and a writer publishes only if the stamp it read is unchanged *while it holds the lock*.
  3. **Alternatively**, make the ledger ref the authoritative state and the check a projection of it.
  4. **Wording:** replace "compare-and-set" in the spec.

#### R3-10 · A filtered event evicts the real pending run in both concurrency groups
- **Location:** `02` §5 (Concurrency: "label, review-ready and base-change events queue rather than cancel"; "the surviving job subsumes the replaced one"); R-01 (the `edited` filter); `02` §5 (event loops); T12; ADR-20.
- **Confidence:** VERIFIED (semantics); REASONED (consequences).
- **Attack:** `types:` cannot filter on `changes.base`, a comment's body or a label's name. Those filters are job-level `if`s that run after the run has already joined the group and replaced the pending entry. That gives two failures:
  - **Pipeline group.** A synchronize run is in flight. The retarget run (`edited` with a base change) waits pending behind it. A title edit evicts it, and the title-edit run's jobs are then filtered out. This feeds R3-03.
  - **decide-only group.** The owner's `/assay override` waits pending behind a running decide-only. It is evicted by the App's own `assay:needs-owner` label event, or by any later ordinary comment. The survivor is filtered out, so it "subsumes" nothing.
- **Impact:** owner commands are silently dropped (QA-08 is missed); base changes are never re-decided; the R3-03 window becomes permanent.
- **Patch:**
  1. **Derive the group key from the filter,** so that events which will be filtered out never enter the real group:

     ```yaml
     group: ${{ (github.event_name == 'issue_comment' && github.event.issue.pull_request && startsWith(github.event.comment.body, '/assay ') && github.event.sender.type != 'Bot') && format('assay-recheck-{0}', github.event.issue.number) || format('assay-noop-{0}', github.run_id) }}
     ```

     Apply the same approach to label events. In the pipeline, an `edited` event without `changes.base` goes to a noop group.
  2. **Add `queue: max`** to the decide-only group. It is allowed there because that group does not cancel in progress.
  3. **Template hygiene:** these expressions stay in `concurrency:`, never in `run:` (T19).

### Medium

#### R3-11 · `output.text` is one mutable slot shared by `open` and `decide`, and annotations accumulate
- **Location:** `02` §1 (`open` box), `02` §5 (per-PR state, pointer); `03` §5 (requeue ≤ 3); `04` §6 (annotations ≤ 50); `06` §4. Acceptance criterion of #103: "Requeue count persists across runs".
- **Confidence:** VERIFIED that annotations append; UNSURE whether a PATCH that omits `text` preserves it.
- **Issue:** `open` re-stamps the same run on every run for a head. A PATCH must resend `output.title` and `output.summary`, and replaces `text` unless the writer merges it (through a read-then-write; see R3-09). Each re-stamp therefore erases the bundle pointer. A decide-only during that window finds no bundle and returns `bundle-expired`, which "requires a re-run". The requeue counter has no specified home and is erased the same way.

  Annotations are appended on every update, so the single long-lived check accumulates stale annotations from every earlier decision, including findings that were dismissed or waived.
- **Patch:**
  1. **Move the state into the ledger** (behind the R3-09 lock): push records, pointers, requeue counters and retry grants. `output.text` becomes a rendered projection.
  2. **Drop `output.annotations` from the verdict check.** Inline comments and the non-required `assay / gate: <name>` checks carry location data instead.

#### R3-12 · An override is not bound to the reasons it was granted for
- **Location:** `04` §5 (`override` → `neutral`), R-08(b), ADR-22, `06` §7.
- **Confidence:** VERIFIED (spec text).
- **Attack:**
  1. The owner overrides a `required-role-degraded` or `infra-llm-unavailable` failure, both of which are LLM infrastructure problems.
  2. A same-head re-run (requeue, retry) later produces new reasons, or a waiver is withdrawn and a gate failure reappears.
  3. The override, still after the push clock, neutralises all of it.

  The Weigh-In ack was bound to a digest for exactly this reason (`04` §9 rule 2); the override was not. An agent can also *induce* the infrastructure failure, by making a required role exhaust its cap (8b), to train override habits.
- **Patch:**
  1. **Bind it to a digest:** `/assay override <reasons_digest> <reason>`, where the digest covers the sorted current reason codes and is printed in the summary. The override is void if any reason is *added*.
  2. **Scope it:** `override llm` neutralises only rule 8. `override all` is the break-glass and is reported separately.

#### R3-13 · The privileged actions have contradictory time scopes
- **Location:** `04` §5 ("a comment-based action must be later than the same clock"), `04` §6 and `04` §7 ("Dismissal suppresses … for the life of the PR"; "A dismissed fingerprint stays suppressed for the life of the PR"), R-08, `04` §9 rule 2. Acceptance criterion of #100: "Dismissed fingerprint stays suppressed across runs".
- **Confidence:** VERIFIED (contradiction).
- **Issue:** after a new push, a dismissal comment predates the new push clock, so §5 voids it while §6 and §7 keep it. Waivers re-arm on every push (2.5 runs per PR), which is owner toil. The spec defines neither retry's scope nor whether an eligible-at-the-time dismissal survives the owner later becoming a pusher.
- **Patch:** add one table to `04` §5, filled in as below. Lifetime is per head, except for dismissals.

  | Action | Lifetime | Bound to |
  |---|---|---|
  | approve | head | `commit_id == head_sha` |
  | waive | head | finding fingerprint and gate-document digest; carried to a new head only if both are unchanged |
  | override | head | reasons digest (R3-12) |
  | dismiss | PR | fingerprint, class and severity ceiling (R3-24); eligibility judged at record time |
  | retry | head | one-shot grant (R3-14) |
  | weigh-in-ack | head | set digest |

#### R3-14 · `/assay retry` has no mechanism, and the obvious one is forgeable
- **Location:** `04` §5 rule 8b; `06` §1 (role budget exhaustion); `06` §5 F2; `05` §4 (decide-only has neither `actions: write` nor the model secret). Acceptance criterion of #101: "Second retry on the same head is refused".
- **Confidence:** VERIFIED that the gap exists; REASONED for the abuse.
- **Issue:** decide-only cannot run a model and cannot dispatch, so retry needs a new pipeline run. The obvious route is a `workflow_dispatch` input such as `retry_role=bugs` (×1.5). Any writer can dispatch (§3), so any writer could award themselves a ×1.5 run, or a larger factor, on any PR, as often as they like. The spec also does not say how a one-role run combines with the other roles' evidence from the earlier run.
- **Patch:**
  1. **Record the grant.** decide-only writes `retry/<repo_id>/<pr>/<head>/<role>` to the ledger as an atomically created ref, which makes it one-shot.
  2. **Dispatch carries the PR number only.** The 2-hourly `requeue` job, or a release event, dispatches with no other input.
  3. **`admit` consumes the grant** by compare-and-set and applies ×1.5 only when a grant is present.
  4. **The retry run re-runs all required roles** on the same evidence key. Combining evidence across runs is out of scope.

#### R3-15 · Any writer can open the requeue channel, and requeue spends quota for nothing
- **Location:** `06` §1 ("Requeue is event-driven first (any later PR event, a decide-only command, an `assay:requeue` label) plus a `schedule` …"; "lists labelled PRs"); `05` §4 (only `requeue` holds `actions: write`); `05` T9 ("rate-limited by actor"); T12; `02` §5 (evidence reuse).
- **Confidence:** REASONED.
- **Issue:** four problems combine.
  - **(a) No mechanism.** Under the permission matrix, a requeue triggered by a decide-only command or label cannot happen: decide-only cannot dispatch.
  - **(b) Labels as state.** The `requeue` job selects PRs by the `assay:llm-pending` label. Any writer can add it to twenty PRs, and every two hours each one gets a full LLM run, until the $40 daily cap is gone. After that, every repo's LLM stage defers for the rest of the window. A writer can also *remove* the label from someone else's deferred PR, so it is never requeued.
  - **(c) No store for the rate limit.** "Rate-limited by actor" has nowhere to keep its counts.
  - **(d) No reuse check.** `admit` never checks whether a completed evidence set already exists for the evidence key, so a re-run of a fully reviewed head spends again.
- **Patch:**
  1. **Requeue from deferral records.** The `requeue` job selects PRs from the deferral records `admit` writes to the ledger (`{repo, pr, head, count, resets_at}`), never from labels. Labels are a projection only.
  2. **Commands and labels flag, never dispatch.** A decide-only command or label at most sets a flag in the ledger record.
  3. **`admit` reuses before it spends.** Its first step: if an App-recorded complete evidence bundle exists for the evidence key (R3-17), reuse it and spend nothing.
  4. **Keep per-actor command counters in the ledger,** checked first in the job.

#### R3-16 · The run key is not unique per run
- **Location:** `03` §4 (`run_key = evidence_key`); `06` §1 (lease `{run_key,…}`; record `{run_key,…}`; reconcile back-fill); `02` §5 (`pending:<run_id>`).
- **Confidence:** VERIFIED (spec text).
- **Issue:** a requeue on the same head, a re-run (R3-05) and a retry all share one evidence key. The consequences:
  - Leases keyed by `run_key` can be released by the wrong run.
  - Ledger dedupe keyed by `run_key` either drops real runs or double-counts back-fills.
  - Each consumer repo runs its own `reconcile` against the one shared ledger, so they race to back-fill the same expired lease.
  - `pending:<run_id>` cannot tell attempts apart.
- **Patch:** add `run_uid = (repo_id, run_id, run_attempt)` to the run description, the lease, the ledger record and the stamp, and keep `evidence_key` for reuse only. Exactly one designated `reconcile` (in the ledger repo itself) reaps leases and back-fills; the per-repo `reconcile` jobs touch only their own checks.

#### R3-17 · Evidence reuse does not define "complete", so draft and deferred runs share keys with LLM-bearing runs
- **Location:** `02` §5 (evidence key, reuse "through a digest stored in an App-authored record"); `04` §2.4 (drafts run gates only); `03` §5 (deferral).
- **Confidence:** REASONED (the spec is silent; the risk is in the implementation).
- **Issue:** the evidence key has no draft flag, tier or "LLM ran" component. Two cases follow:
  - A draft run (gates only) and the `ready_for_review` run on the same head share a key. If the draft's set is reused as "complete", the LLM never runs.
  - A deferred run and its requeue share a key. If reuse treats the deferred set as complete, the requeue does nothing until the PR fails after three attempts.
- **Patch:** a reusable evidence record carries `{evidence_key, llm: complete|skipped(draft)|deferred|degraded, tier}`. Only `complete` with a tier at least as high as the needed one is reusable for the LLM part; gate results are reusable regardless.

#### R3-18 · The approval poll is not costed and can be delayed or dropped
- **Location:** `04` §6 ("the poll picks the approval up within 20 minutes"); QA-08; `06` §3 (480 minutes of polling counted for `requeue` + `reconcile` only).
- **Confidence:** VERIFIED for the arithmetic and the schedule behaviour; UNSURE for the `workflow_run` alternative, which needs a canary.
- **Issue:** a poll every 20 minutes is 72 runs a day, or ≥ 2,160 billed minutes per repo per month at the one-minute minimum. That is uncounted: about $13 per repo per month, or about $39 for the three repos in H-03, which is +50–80% on the stated $48–80. GitHub also documents that scheduled runs are delayed under load and that some queued jobs may be dropped, so the 20-minute bound in QA-08 is not a guarantee.
- **Patch:**
  1. **Cost the poll** in `06` §3, or poll hourly at an off-peak minute.
  2. **Replace the poll with a chain:** a trigger-only workflow on `pull_request_review` (no permissions, no secrets; PR-controlled, which is acceptable because it carries no data) is the target of a `workflow_run` workflow on the default branch. That workflow re-decides from current API state, giving about 1–2 minutes of latency.
  3. **The chain is only a nudge.** A PR can delete the trigger workflow, so the chain cannot be the only path, and its payload is never trusted.

#### R3-19 · Two PRs with the same head into the same protected base share one verdict
- **Location:** `02` §5 ("exactly one App-authored check run per (head SHA, `assay / verdict`)"); `04` §5 (per-PR facts: author, approvals, commands).
- **Confidence:** REASONED. GitHub allows two open PRs from different head branches that point at one SHA.
- **Issue:** the author, approvals, commands and labels are per PR, but the verdict is per SHA, and the last writer wins. An override, waiver or approval-flip decided on PR B lands on PR A's required check. This is lower risk after R3-01 and R3-02, but it is still a context confusion.
- **Patch:** `decide` lists `GET /commits/{sha}/pulls`. If more than one open PR targets the same protected base, it publishes `failure ambiguous-head`. Two PRs on one head are never legitimate in this workflow.

#### R3-20 · The ledger repo and the replicated App key are outside doctor's reach
- **Location:** `06` §1 (ledger repo, separate installation); H-14; `05` §7; `05` §1 (assets: "Persisted bundles / events / ledger").
- **Confidence:** REASONED.
- **Issue:** if agent machine users are org members whose base permission is write, they can do three things to the ledger repo:
  - delete slot refs, which breaks the concurrency limit;
  - create them, which is a denial of service;
  - rewrite cost records, which lifts the daily cap.

  Nothing checks this. Separately, the single App key sits in the `assay-publish` environment of every consumer repo, and any copy can mint tokens for every installation, ledger included. One repo created with a lax environment (R3-04) therefore compromises all of them.
- **Patch:** doctor asserts that the ledger repo's collaborators are the owner and the App only, that a ruleset restricts `refs/heads/**` updates to the App, and that agent ids have no access. Consider a separate *ledger* App installed only on `assay-ledger`, so that a verdict-key leak in one repo cannot rewrite budget state.

#### R3-21 · The design depends on reopening a completed check run, which is unverified
- **Location:** `06` §4 step 1 (`open` finds the single run and sets it `in_progress`); `02` §3 (a retarget "overwrites the single App check run"); A-27.
- **Confidence:** UNSURE.
- **Issue:** the one-run-per-SHA design needs PATCH `status: in_progress` on a run that has already `completed`, and needs GitHub to clear the earlier conclusion for the required-check evaluation. The REST description neither documents nor forbids it.

  If it is rejected, `open`'s re-stamp fails, and the old `success` stays visible to merge for the whole of the new run, which is R3-03 at a wider scale. If the old conclusion is kept while the status changes, the required-check result is undefined.
- **Patch:** add it to the M1 canary list, next to A-27. If it fails, `open` must instead publish a completed `failure (re-deciding)` before starting, which is fail-safe.

### Low

#### R3-22 · Commit attribution reads a list capped at 250 commits
- **Location:** `04` §5 (b); `02` §4.
- **Confidence:** VERIFIED.
- **Issue:** `GET /pulls/{n}/commits` returns at most 250 commits. Rule (b), computed from that list, misses committers beyond it, so an owner's commits can hide past the cap.
- **Patch:** compute the range from git objects in Z4 (`git rev-list base..head`) and resolve attribution per commit. More than 250 commits ⇒ `needs-split`.

#### R3-23 · Approval selection must use each reviewer's latest review state
- **Location:** `04` §5.
- **Confidence:** REASONED.
- **Issue:** an "APPROVED then CHANGES_REQUESTED" sequence on the same SHA must not count.
- **Patch:** use, for each reviewer id, the latest review on `head_sha` whose state is not `COMMENTED`. It must be `APPROVED` and not dismissed.

#### R3-24 · The command grammar and lists are inconsistent
- **Location:** R-01 (lists `recheck`, `waive`, `ack`, `override`); `02` §1 (the same four); `04` §6 (`recheck`, `waive`, `ack`, `override`, `retry`, `dismiss`); `04` §9 (`weigh-in-ack`); #61 (`ack` and `weigh-in-ack`).
- **Confidence:** VERIFIED.
- **Issue:** the four places disagree on the command set.
- **Patch:** a closed grammar:
  - one command per comment, on the first line;
  - not inside a blockquote or code fence (so a quote-reply of agent text is not a command);
  - exact tokens, so `ack` is renamed `weigh-in-ack` everywhere;
  - unknown commands rejected, with a reply.

#### R3-25 · Fuzzy dismissal crosses severity and class
- **Location:** `03` §3 (fuzzy match: same file, same symbol, similarity ≥ 0.8); `04` §6.
- **Confidence:** REASONED.
- **Issue:** a dismissed nit in `fn foo` suppresses, for the life of the PR and before the verifier, a later blocker in `foo` with similar evidence.
- **Patch:** a dismissal matches only the same `class`, with severity ≤ the dismissed severity; a higher severity is published with "previously dismissed similar (minor)".

#### R3-26 · Revocation is slow because eligibility lists come from the run's `base_sha`
- **Location:** `04` §5; `02` §5 (bundle snapshot).
- **Confidence:** REASONED.
- **Issue:** a decide-only on an old bundle still honours an approver removed from `.assay/owners` since then.
- **Patch:** decide-only reads `owners`, `approvers` and `agents` from the *current* tip of the protected base (the owner controls it, and fresher only removes rights) and records them in the decision bundle.

#### R3-27 · Wording: the workflow file now always comes from the default branch, and the budget read has small gaps
- **Location:** P2; ADR-03; `02` §3; A-28; `06` §1 (budget read); `04` §2.4.
- **Confidence:** VERIFIED (§3); REASONED (budget).
- **Issue:**
  - **Workflow source.** For a PR into a protected base other than the default, the workflow file comes from the default branch while `.assay` is read at `base_sha`. An older release branch can then pin an assay digest that expects a different job topology.
  - **Budget read.** Separately, the read is check-then-act. Two concurrent `admit`s can overshoot the cap by up to two reservations, which is acceptable but should be stated. "Live lease" must say whether an expired but unreaped lease counts; it should count until reaped.
  - **Tier degrade.** `admit`'s full→lite degrade must be carried in `admit`'s outputs and accepted by Z4's required-role recomputation; otherwise it reads as a missing role.
- **Patch:**
  1. Reword P2 and ADR-03 for the workflow source.
  2. Doctor fails when a protected base's `[assay].digest` differs from the default branch's, or requires a compatibility matrix.
  3. State the overshoot bound and the lease-counting rule, and pass the degrade through `admit` outputs.

#### R3-28 · Any writer can delete the run bundle
- **Location:** `04` §1 S9; F12; F19; `02` §5.
- **Confidence:** REASONED. Deleting an artifact needs `actions: write`, which the write role grants.
- **Issue:** an agent deletes the bundle artifact of any PR's run. Its decide-only then returns `bundle-expired` and needs a paid re-run, a cheap way to burn quota and stall another PR's approval.
- **Patch:** distinguish "missing" from "digest mismatch" in F19. Mirror the decision-critical part of the bundle (snapshot, decision, the digests of the gate documents) into the ledger, which writers of the consumer repo cannot touch. A missing artifact then degrades only replay of the model outputs, not re-decision.

## 5. Attacks tried that failed (and one premise of mine that was wrong)

- **Slot acquisition atomicity** holds: `POST /git/refs` fails on an existing ref (A-26, acquire half).
- **The ledger file CAS** is genuine: the Contents API PUT requires the current blob `sha` and fails on a mismatch.
- **Forging the bundle pointer** from a PR-added workflow fails. Only the App writes the pointer, and once R3-04 is fixed no branch workflow can reach the App key.
- **`issue_comment`, `schedule` and `pull_request_target` run the default-branch file** (VERIFIED). My initial premise was that `pull_request_target` runs the PR *base* branch's file, so an agent could create a base branch with a modified workflow. That premise is **wrong** on github.com since GitHub's change, and I withdrew the finding. Only GHES < 3.20 behaves the old way.
- **Tampering with `admit`'s context pack from Z2b** fails, because the digest in `admit`'s outputs authenticates it (`02` §1).
- **A hostile `build.rs` outliving its job** to overwrite the bundle fails: hosted runner VMs are torn down at job end, and the output-digest rule would catch it anyway.
- **The `sender.type != 'Bot'` loop guard** holds for App-authored comments and labels. The breakage is elsewhere (R3-10).
- **Binding the Weigh-In ack to a digest** holds; it is vulnerable only through R3-02.
- **Daily-cap overshoot through the check-then-act read** is bounded by the two slots; it is a low-severity note (R3-27), not an attack.

## 6. Summary table

| ID | Sev | Conf | Area | Rec. | Proposed resolution (location) | Disposition (v1.2 patch location) |
|---|---|---|---|---|---|---|
| R3-01 | crit | VERIFIED/OBSERVED | approver | ✔ | Privileged identity = never agent-reachable; owner's daily account → `maintainers`; H-13 mandatory; H-08 restated to cover comment/review/merge; `performed_via_github_app` filter; bypass only for break-glass account; `01` §2, P10, `04` §5, `05` §7, T9, H-06/H-08/H-13 | ✔ accepted by owner 2026-10-11 · `01` §2, P10; `03` §1 (`maintainers`); `04` §5; `05` A1, T9, §7; `06` §4.5, §8 (H-08 two-part check); `07` ADR-16, ADR-24, A-17, H-06/H-08/H-13 + R3-01 record row |
| R3-02 | crit | VERIFIED | decide-only, approver | ✔ | Commands recorded from the `created` payload into App-authored ledger records; edits/deletes only revoke; `04` §5–6, `02` §1/§4, T9 | ✔ · `04` §5 (commands are events), §6, §9 rule 2; `02` §1, §4, §5; `03` §6 (`CommandRecorded`/`CommandVoided`); `05` T21; `06` §1 (command records), §8 canary; `07` ADR-16, ADR-23 |
| R3-03 | crit | VERIFIED (chain REASONED) | decide-only, approver | ✔ | `base-not-protected` ⇒ `failure`; base-scoped check name for non-default protected bases; decide re-checks `base.ref`; `02` §3/§5, `03` §5, `04` §5, `06` §6, T15 | ✔ · `01` R-01; `02` §1, §3, §5; `03` §1, §5; `04` §5 rule 2, §6; `05` T15, §7; `06` §6, §8 canary |
| R3-04 | high | VERIFIED | decide-only, admit | ✔ | Environment deployment-branch policy = default branch only; doctor checks every repo; dispatch provenance `head_branch == default`; A-28 verified; `05` §5, §7, F19 | ✔ · `05` T7, §5, §7; `02` §5 (evidence pointer, dispatch `head_branch`); `06` F19, §8 canary; `07` ADR-20, A-28 (verified) |
| R3-05 | high | VERIFIED/REASONED | admit, ledger | ✔ | `if: github.run_attempt == 1` on every privileged job from the first template; binary refuses attempts > 1; canary; `06` §1, §3, T12 | ✔ · `02` §1; `04` §1; `05` T22, §7; `06` §8 canary; `07` A-31 |
| R3-06 | high | VERIFIED/REASONED | semaphore, admit | ✔ | `release` job after review; CAS release/reap (force-with-lease); admit self-reaps; superseded runs still release and ledger; count admissions; `slot-busy` ≠ `llm-degraded`; F17 corrected; `06` §1, §4, §5 | ✔ · `06` §1 (release job, CAS release/reap, admit self-reaps, reserved records, `slot-busy`), §4, F17; `02` §1, §5; `03` §5, §8; `04` §1 S1a/S10, §2.4, §5 rule 8a; `05` T12, §4; `07` ADR-19, A-33 |
| R3-07 | high | VERIFIED | approver | ✔ | Append-only per-PR push records (synchronize only) in ledger; clean-merge exclusion applies to (c); push clock from record, never `started_at`; unattributed commits ⇒ owners ineligible; `04` §5, `02` §1/§4 | ✔ · `04` §5 (push records, clean merge in (b) and (c), push clock, update modes, ignored-approval message), §1 S0; `02` §1, §4; `06` §4.1; `07` ADR-16 |
| R3-08 | high | VERIFIED | decide-only | ✔ | Two-part pointer (evidence incl. `workflow_dispatch`; decision from decide-only); decide-only persists a decision bundle; `02` §5, T11, F19, QA-11 | ✔ · `02` §1, §5 (two-part pointer, decision bundle); `01` QA-11; `05` T11; `06` F19; `07` ADR-09, ADR-20 |
| R3-09 | high | VERIFIED | decide-only | ✔ | Per-(repo, head) publish mutex in ledger; monotone stamp; drop "CAS" wording; `02` §5, `06` §4, T10 | ✔ · `02` §5 (per-head publish lock, stamp `pending:<run_id>:<seq>`); `04` §1 S8; `05` T10; `06` §1, §4.3, F21; `07` ADR-23, A-27 |
| R3-10 | high | VERIFIED | decide-only | ✔ | Filter-derived concurrency group keys; `queue: max` for decide-only; `02` §5, T12 | ✔ · `02` §5 (filter-derived group keys, `queue: max`); `05` T12; `06` §8 canary; `07` ADR-20 |
| R3-11 | med | VERIFIED/UNSURE | decide-only, ledger | ✔ | State (pointer, push records, counters, grants) in ledger; `output.text` is a projection; no annotations on the verdict check; `02` §5, `04` §6 | ✔ · `02` §5 (ledger state, no annotations); `04` §6; `03` §5 (requeue counter); `07` ADR-23 |
| R3-12 | med | VERIFIED | decide-only | ✔ | Override bound to a reasons digest; `override llm` vs `override all`; `04` §5, R-08 | ✔ · `04` §5 (override line, lifetime table); `01` R-08; `06` §7; `07` ADR-22 |
| R3-13 | med | VERIFIED | decide-only, approver | ✔ | One lifetime/binding table for all privileged actions; `04` §5–7 | ✔ · `04` §5 (lifetime/binding table), §6, §7 |
| R3-14 | med | VERIFIED/REASONED | decide-only, admit | ✔ | One-shot retry grant ref in ledger; dispatch carries PR number only; admit consumes grant; `04` §5 8b, `06` §1 | ✔ · `06` §1 (retry grant ref, dispatch PR number only, admit consumes); `04` §5 rule 8b, S1a; `03` §4, §6 |
| R3-15 | med | REASONED | admit, decide-only | ✔ | Requeue from ledger deferral records, not labels; commands only flag; admit reuses complete evidence before spending; per-actor counters in ledger; `06` §1, T12 | ✔ · `06` §1 (deferral records, flags only, per-actor counters); `02` §5 (admit reuses first); `04` §6 grammar; `05` T9, T12 |
| R3-16 | med | VERIFIED | ledger, semaphore | ✔ | `run_uid = (repo_id, run_id, run_attempt)`; single ledger-side reconcile for leases/back-fill; `03` §4, `06` §1, §4 | ✔ · `02` §5 (`run_uid`); `03` §4, §6; `06` §1 (ledger-side reconcile), §4.4; `05` §4 |
| R3-17 | med | REASONED | admit | ✔ | Evidence completeness record (`llm: complete/skipped/deferred/degraded`, tier); `02` §5 | ✔ · `02` §5 (evidence record with `llm`, `tier`) |
| R3-18 | med | VERIFIED/UNSURE | decide-only (cost) | ◐ | Cost the poll or poll hourly off-peak; `pull_request_review` → `workflow_run` nudge after M1 canary; `04` §6, `06` §3, QA-08 | ◐ accepted narrowed: hourly off-peak poll, costed; `workflow_run` nudge staged behind M1 canary · `04` §6; `01` R-01, QA-08; `06` §3, §8; `07` A-32 |
| R3-19 | med | REASONED | decide-only | ✔ | `failure ambiguous-head` when > 1 open PR on one head into one protected base; `02` §5 | ✔ · `02` §5; `04` §5 rule 2a; `03` §5 (`ambiguous-head`); `05` T10 |
| R3-20 | med | REASONED | ledger | ◐ | Doctor checks ledger access and refs ruleset; consider a separate ledger App; `06` §1, `05` §7, H-14 | ◐ accepted: doctor checks ledger access and refs ruleset; separate ledger App left as open question · `05` T23, §7; `06` §1; `07` H-14, §5 |
| R3-21 | med | UNSURE | decide-only | ◐ | M1 canary for completed→`in_progress`; fallback `failure (re-deciding)` first; `06` §4, A-27 | ◐ accepted: M1 canary + fail-safe fallback · `06` §4.1, §8; `04` §1 S0; `03` §5 (`re-deciding`); `05` T10; `07` A-30 |
| R3-22 | low | VERIFIED | approver | ✔ | Attribution from git range in Z4; > 250 commits ⇒ `needs-split`; `04` §5 | ✔ · `04` §5 (range from git, > 250 ⇒ `needs-split`); `02` §4 |
| R3-23 | low | REASONED | approver | ✔ | Latest non-comment review per reviewer must be APPROVED; `04` §5 | ✔ · `04` §5 (approvals); `02` §4 |
| R3-24 | low | VERIFIED | decide-only | ✔ | Closed command grammar; one name per command; R-01, `02` §1, `04` §6, §9 | ✔ · `04` §6 (closed grammar), §9; `01` R-01; `02` §1 |
| R3-25 | low | REASONED | dismiss | ✔ | Dismissal matches same class and ≤ dismissed severity; `03` §3, `04` §6 | ✔ · `03` §3; `04` §6, §7 |
| R3-26 | low | REASONED | approver | ◐ | Read eligibility lists from the current protected-base tip in decide-only; `04` §5 | ◐ accepted narrowed: decide-only intersects base_sha lists with current-tip lists (removal immediate, addition waits for a new run) · `04` §5; `02` §4 |
| R3-27 | low | VERIFIED/REASONED | admit | ✔ | P2/ADR-03 wording; digest-consistency check across protected bases; state budget overshoot bound and lease counting; degrade passed via admit outputs | ✔ · `01` P2; `02` §3; `07` ADR-03; `05` T3, §7 (digest consistency); `06` §1 (overshoot, lease counting); `03` §4, `04` §2.4 (degrade via admit outputs) |
| R3-28 | low | REASONED | decide-only | ◐ | Distinguish missing vs mismatched bundle; mirror decision-critical bundle part into ledger; F12, F19 | ◐ accepted: missing vs mismatched bundle distinguished (F20); ledger mirror left as open question · `06` F12, F19, F20; `05` T11; `01` QA-11; `07` §5 |

Counts: **28 findings: 3 critical, 7 high, 11 medium, 7 low.** Recommended triage: ✔ 23 and ◐ 5 (R3-18, R3-20, R3-21, R3-26, R3-28); none ⏸ or ✘. **Disposition:** as recommended; all 28 patched in v1.2 (the ◐ ones narrowed as stated in the Disposition column). R3-01 changed owner decisions H-06/H-08/H-13 and was accepted by the owner on 2026-10-11 (`07 §3`). Story acceptance-criteria changes are listed in §7 and in detail in the Story AC follow-up file for issue #35. One premise of my own was withdrawn after verification (§5).

**Convergence.** Three independent lines point to the same root cause, the per-SHA check run used as a mutable state store (R3-03, R3-07, R3-08, R3-09, R3-11):

- the platform offers no atomic update;
- the check's output is overwritten as a whole;
- the check cannot be scoped to one base or one PR.

The recurring patch is the same: move authoritative state (push records, command records, pointers, counters, grants, a per-head publish lock) into the App-only ledger repository, whose git refs *do* support compare-and-set, and treat the check run as a projection of that state.

## 7. Stories needing acceptance-criteria changes

| Story | Findings | AC to add or change |
|---|---|---|
| #55 plan | R3-16, R3-27 | `run.json` carries `run_uid` and `base_ref`; validates against the updated schema |
| #56 decide (authenticate, recompute) | R3-03, R3-05, R3-16, R3-19 | Base-ref change ⇒ write nothing; `run_attempt > 1` ⇒ refuse; > 1 open PR on the head into one base ⇒ `failure ambiguous-head` |
| #57 decide (publish, persist) | R3-03, R3-06, R3-08, R3-09, R3-11 | `base-not-protected` is `failure`; publish under the per-head lock (a concurrent-writer test); superseded runs still ledger and release; two-part pointer; no annotations on the verdict; pointer survives a same-head `open` |
| #58 P10 | R3-01, R3-02, R3-07, R3-22, R3-23, R3-26 | Owner's daily account not privileged; edited or deleted comments grant nothing; "Update branch" (merge) keeps eligibility under rules (b) **and** (c); a non-push event never changes the pusher set; the push clock is not moved by a re-run; a > 250-commit range is handled; latest review state wins |
| #59 waive / override | R3-02, R3-12, R3-13 | Override bound to the reasons digest (an added reason voids it); `override llm` ≠ `override all`; waiver lifetime per the R3-13 table |
| #61 decide-only workflow | R3-02, R3-04, R3-08, R3-10, R3-18, R3-24 | Command records in the ledger; environment branch policy asserted; decision bundle persisted; filtered events never evict a pending command (test: label event behind an override); closed grammar; poll cost bounded |
| #70 ledger | R3-02, R3-06, R3-07, R3-16, R3-20 | Records keyed by `run_uid`; `reserved` records at admission; push and command record streams; access asserted by doctor |
| #71 semaphore | R3-06, R3-16 | Release and reap by compare-and-set (a stale holder cannot delete a newer lease); a lease is released by the `release` job after review; TTL ≥ review timeout |
| #72 admit | R3-05, R3-06, R3-15, R3-17, R3-27 | Refuses `run_attempt > 1`; self-reaps expired leases; `slot-busy` is distinct from quota deferral; reuses complete evidence before reserving; cooldown counts admissions |
| #73 reconcile | R3-06, R3-16 | A single ledger-side reconciler; back-fill deduplicated by `run_uid`; F17 latency stated truthfully |
| #100 dismiss | R3-02, R3-13, R3-25 | Dismissal from a command record only; survives pushes per the R3-13 table; does not suppress a higher severity or another class |
| #101 retry | R3-05, R3-14 | One-shot ledger grant; dispatch carries no cap or role input; re-running the `review` job by hand spends nothing |
| #103 deferral | R3-06, R3-08, R3-11, R3-15 | Requeue count lives in the ledger; a requeued run's bundle is accepted by decide-only; selection from deferral records, not labels; `slot-busy` path |

Outside the listed Stories: the `open` job (push records, never touching `started_at`), the workflow-template Story (`run_attempt` guard, concurrency keys) and the doctor Story (environment policies, ledger access, bypass list) need matching acceptance criteria. Add these canaries to M1:

- re-run of the `review` job;
- retarget plus immediate merge;
- a label event behind a pending override;
- editing an owner's comment into a command;
- reopening a completed check run;
- a stale holder deleting a newer lease.
