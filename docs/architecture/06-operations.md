# 06 · Operations: auth, quota, cost, failure modes, rollout

Status: **v1.1**. Rollout phases are named **M0–M6** (milestones) so they do not collide with principles P1–P10.

## 1. Auth and quota model

- **Mechanism.** `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`. Preflight runs inside the `review` job (the only job with the secret): `claude auth status` (JSON by default; no `--json` flag assumed), require `authMethod == "oauth_token"`; assert `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and provider variables are unset (an API key outranks the OAuth token and silently bills the API); verify the `system/init` message of the first call shows no MCP servers, plugins, skills or agents, and pass the hook canary (`03 §8`; `init` has no hooks field) (`init-unclean` otherwise).
- **What "cost" means.** The CLI's `total_cost_usd` is a client-side estimate at list prices, not billing. On a subscription nothing is charged; assay treats it as **AEU** (API-equivalent units), a relative measure of consumption. AEU is **not** a quota: the Max limit is a rolling 5-hour window plus a weekly window, shared across all Claude surfaces including the owner's own agent sessions, with model-specific limits.
- **Two series are tracked.** (1) AEU per run/tier/role (relative cost, drives caps). (2) Observed rate-limit state from `rate_limit_event` messages in the `stream-json` output (`RateLimitObserved`), per subscription. The event carries `status: allowed|allowed_warning|rejected` and fires with `allowed` routinely, so assay **pauses the LLM stage** on `rejected` or `allowed_warning` only, classifying the state `QuotaExhausted` and deferring (below). A `setup-token` OAuth token carries only the `user:inference` scope and cannot read plan utilisation, so there is **no utilisation-based pause** (v1's "≥ 70%" rule is removed, `A-13`). Because review and authoring share one window and are correlated (a burst of agent PRs is a burst of reviews), a daily dollar cap alone does not protect the window; the pause-on-signal rule and the semaphore do.
- **Terms (`A-01`).** Max limits assume ordinary individual usage; Anthropic reserves enforcement. This design is for the owner's own private repositories only. Tripwires: terms change; throttling of CI use; the reported announcement that headless/Agent-SDK/GitHub-Actions usage moves to a separate monthly credit (**unverified**: only secondary sources were reachable; check the official support article at M0 sign-off and quarterly). Response: set `runner = "claude-api"`; monthly cost at API prices is bounded by the AEU estimates in `01 §8` (≈ $282–$423 at 10–15 PRs/day, a lower bound).

### Caps

| Cap | Value | Basis / enforcement |
|---|---|---|
| Per role call | `Cap (AEU)` in `04 §3` | `--max-budget-usd = cap` exactly, plus `--max-turns` and a wall-clock timeout. The flag is a client-side estimate that "can pass the cap by up to one response"; `total_cost_usd` is emitted only in the final `result` message, so mid-run killing on assay's own accounting is not possible (SIGTERM yields exit 143 and loses both output and cost). The real cost of every call, including `error_max_budget_usd` / `error_max_turns` results (which carry `total_cost_usd` and `modelUsage` but no structured output), is ledgered |
| Per run | sum of role caps: micro $0.25; lite **$0.74**; full $2.30; full + `meta` $2.60; sharded full ×shards (≤ 3): $6.90 / $7.80 | remaining roles skipped when the run cap is reached |
| Per PR per hour | 3 runs, then deferred | T12 |
| Per day, per subscription | **$40 AEU** (`A-13`, provisional) | ≈ 2.1× expected spend at 15 PRs/day ($19.2); leaves room for interactive use. (v1's $30 was only 1.56× the corrected load) |
| Eval ledger | $150 AEU/month, separate | `04 §8`; never draws on the production cap |
| Concurrency | **2 model-running `review` jobs across all repos** (semaphore below); ≤ 4 model processes per run, shards sequential | `A-03`, `ADR-19` |

`A-13` is provisional. Calibration (M2 for lite/micro, **M3 for full**, ≥ 7 days wall clock to see the weekly window) replaces it with measured values: AEU per tier, window consumption, and behaviour at limits. Measurement is done by observing production-shaped load off-hours at ≤ 50% of the guardrail; deliberately driving the account to throttle is avoided because it consumes the owner's interactive quota.

### Deferral and requeue

Two different conditions, two different handlings (rubric rule 8):

- **Deferral** (quota or infra: `budget-deferred`, `infra-llm-unavailable`): records `BudgetDeferred`, shows the reason in the check summary and sets label `assay:llm-pending`. Trust-path/exec-surface PRs stay `in_progress` (never `neutral`, which counts as passing for required checks), requeued at most 3 times, after which they fail with `infra-llm-unavailable` so a stuck PR is visible. Others get `success` with a visible warning (`llm-degraded`). Requeue is **event-driven first** (any later PR event, a decide-only command, an `assay:requeue` label) plus a `schedule` every 2 hours (job `requeue`) that lists labelled PRs and dispatches the base workflow's `workflow_dispatch` entry (`actions: write` held only by this tiny base-defined job; inputs: PR number, which the workflow re-resolves to the current head SHA and re-validates as untrusted). Backoff follows `resetsAt` from a `rejected` rate-limit event when present. The dispatch path reuses the same Z0–Z4 topology.
- **Role budget exhaustion** (a required role hit its own cap, turns or timeout): waiting cannot heal it, so it is `failure (required-role-degraded)` on trust-path/exec-surface PRs, with `/assay retry` (one re-run with that role's cap ×1.5, recorded) or `/assay override` as the remedies; on other PRs it is a `llm-degraded` warning.

### Ledger and semaphore (central, ADR-19)

GitHub has no API to list check runs by time or app (they are per commit, so force-pushed or superseded SHAs are undiscoverable) and Actions concurrency groups are repository-scoped, so neither the v1 "sum `assay / cost` check runs" ledger nor a cross-repo "2 at a time" rule was implementable. The ledger is therefore a small private repository, `ledger_repo` in base config (`Wet-Ink-Corporation/assay-ledger`), to which the assay App has `contents: write` through a separate installation token (cross-org repos need their own installation):

- **Cost records.** `ledger/YYYY-MM.jsonl`, append-only, written by `decide` (and by `reconcile` for runs that never reached `decide`, charged at the run cap with `cancelled: true`) with a compare-and-set on the file `sha`: `{run_key, repo, pr, head_sha, tier, roles[{role, aeu}], aeu, cancelled, at}`.
- **Budget read.** `admit` (not `plan`, which has no credentials) sums the last 24 h of records plus the `reserved_aeu` of live leases. It is synchronous GitHub state, current to within the runs in flight.
- **Semaphore.** Slots are refs `refs/heads/slot-0` and `refs/heads/slot-1` in the ledger repo. `admit` acquires one by creating the ref (creation fails if it exists, which is atomic) pointing at a commit whose message carries the lease `{run_key, repo, pr, reserved_aeu, expires_at}` (TTL 45 min); `decide` deletes it (`SlotReleased`); `reconcile` reaps expired leases. No slot free ⇒ `admit` waits up to 10 min, then defers (`budget-deferred`).
- **Reservations** are made only by `admit` and only for runs that will spend (not trivial, draft or needs-split runs, which v1 wrongly reserved), and are released by `decide`. v1's superseded SHAs left phantom $1.00 reservations until reconcile; superseded checks are now closed `cancelled` and their leases released.
- The ledger is not telemetry: if it cannot be read, `admit` fails and the LLM-unavailable policy applies; if it cannot be written, `decide` warns (`LedgerWritten` absent) and `reconcile` back-fills from bundles.

## 2. Cost model (per run, AEU)

| Tier | Roles | Expected | Cap |
|---|---|---|---|
| trivial | none | $0 | $0 |
| micro | `combined-micro` | $0.15 | $0.25 |
| lite | `combined` + ≤ 3 verifier calls (3 × $0.08) | $0.50 | **$0.74** |
| full | `bugs`, `security`, `design`, coordinator, ≤ 5 verifier calls | $1.50 | $2.30 |
| full + trust path | + `meta` | **$1.70** | $2.60 |
| full, sharded (≤ 3) | the above per shard, shards sequential | ≤ 3× | $6.90 ($7.80 with `meta`) |

Expected values are assertions until M2/M3 measure them (`A-21`); the caps are provisional for the same reason. One check on plausibility from the reviewers: a full-size 120 KB context pack (~30k tokens) plus ~25k tokens of system prompt/tools, grown over 25 turns, reads on the order of 1.9M cache-read tokens, which at list prices is $0.56–$0.94 before cache writes. A `bugs` cap of $0.60 may therefore be too small for large packs; large packs shard, and the cap is set from the measured p90.

Per PR (all runs): 1 first run at the tier cost plus 1.5 further runs, **each at 100%** because specialists review the full cumulative diff (`04 §7`) ⇒ factor 2.5 (v1 assumed 50% for incremental runs, which nothing in the design supported). Mix 20/25/35/20 (`A-02`): $0.5125 × 2.5 = **$1.28/PR** ⇒ $12.8/day at 10 PRs, $19.2 at 15, **$282–$423/month AEU**. This is a lower bound: the mix prices every `full` PR without `meta` (trust-path PRs are always `full`+`meta`, expected $1.70, so up to ≈ $0.10/PR averaged is missing if trust-path PRs are the whole 20%), and sharded runs are ignored. If trust-path PRs are more than the 20% assumed, the mix is revised from M1 history.

Levers if over budget, in order: (1) raise the `micro` floor; (2) merge `design` into `bugs`; (3) skip the verifier for `lite` majors; (4) **incremental specialists**: review only changed hunks since the last completed run plus a full-diff verifier pass (brings the factor toward 1.5–1.75 at some recall risk, to be measured); (5) cheaper tier for specialists, stronger tier for coordinator/verifier; (6) `security` only when a risk glob or exec surface matched.

**Prompt cache (design, `A-15`).** The specialists of a run share the *same* fixed system prompt, the *same* cwd (an empty directory; the PR tree is `--add-dir`), the *same* tool block and the *same* findings schema, with `--exclude-dynamic-system-prompt-sections`, so their prefix is byte-identical. The context pack comes first in the user message; the role instruction follows it. The first call goes alone for ~20 s so the others hit its cache entry. The coordinator and verifier use different schemas (and possibly a different model tier, which has its own cache namespace), so they **do not** share it; only same-model, same-schema roles do. Reviewers reported that a per-role cwd plus the default system prompt (which embeds a cwd-derived memory path) would have defeated sharing. Target ≥ 30% input saving on the specialist calls; measured via `cache_read_tokens` at M2.

## 3. CI cost and performance

- **Z2b warm build.** No `actions/cache` for any decision-bearing build (the Actions cache can be pre-seeded under the base scope with the run-scoped runtime token, keys are immutable once created, and `restore-keys` returns the newest match, so a poisoned entry would persist after merge and be restored by every later job, `T6`). Instead, a trusted job on pushes to the default branch builds the workspace's dependency `target/` and registry cache and publishes it as a **digest-pinned OCI artifact** to GHCR (the same mechanism as the assay binary, `A-20`); base `.assay/plugins.toml` names it by digest (`warm_target`); Z2b jobs pull it by digest and run `cargo clean -p <workspace crates>` before building, treating it as untrusted input. A stale digest only costs time (a cold build). If the Actions cache is kept anywhere, only for non-decision steps and only with exact keys that contain a main commit SHA, never `restore-keys`.
- **gates duplicate the repo's own `rust.yml`.** Deliberate (a PR can edit `rust.yml`, so its result is untrusted): assay's Z2b runs base-defined fmt, `baseline --check`, `nextest --no-tests=fail`, doctests and `cargo deny`, split across `gates-lint` and `gates-test`. The repo's own workflow remains as developer feedback.
- **Actions minutes (the only cash cost).** Billed per job rounded up to whole minutes (Linux 2-core $0.006/min; 3,000 included minutes on Team; rates from the reviewers' cited GitHub docs). Estimated billed minutes per run: trivial **4** (open, plan, gates-static, decide, each rounded up), micro 11, lite 20, full 33 (the `admit` job and the `gates-test` split add ≈ 1–3 minutes over v1) ⇒ weighted 0.2×4 + 0.25×11 + 0.35×20 + 0.2×33 = **17.15 min/run**. At 25 runs/day × 22 days = 550 runs ⇒ 9,433 min + 480 (requeue and reconcile polling) + 1,100 (decide-only, ≈ 2 job-minutes per run) = 11,013 min ⇒ overage (11,013 − 3,000) × $0.006 = **$48/month**; at 15 PRs/day ≈ 825 runs ⇒ 14,149 + 480 + 1,650 = 16,279 min ⇒ **$80/month**. (v1's $33–$58 excluded polling, decide-only runs, the trivial 4-job minimum and the new jobs.) Polling schedules are kept to a 2-hour requeue and a 6-hour reconcile instead of two 30-minute crons (which alone would be ≈ 2,880 min/month per repo).
- **Critical-path latency, `full`** (lower-bound sums, optimistic): open 0.5 + plan 1.0 + admit 0.5 + review start 1.3 + first-call stagger 0.3 + slowest specialist (25 turns × ~10 s) ≈ 4.2 + coordinator 1.5 + verifier 2.0 + decide 1.0 ≈ **12.3 min**; with ordinary variance p50 ≈ 15, p95 ≈ 25 (QA-04). `.rs` PRs wait on Z2b in parallel; with the warm `target/` a lint+test run is 5–10 min. **`trivial`**: runner start 1.3 + open 0.5 + plan 1.0 + decide 1.0 = **3.8 min** before any gate work, hence QA-01's p50 ≤ 5 / p95 ≤ 8.
- **Gate timeouts.** The temper adapter's `timeout_s` (1,200) covers only its own steps (scan in `gates-static`, `baseline --check` in `gates-lint`); the declared steps (`fmt`, `deny`, `tests`, `doctests`) carry their own `timeout_s` (`03 §1`). A timeout is `gate-error`; the Z2b jobs retry once on infra-class errors only.
- **Optional hardening (M2 stretch, not a v1 claim).** Run the Z2b job under a separate low-privilege uid or container with no access to the runner's file-command directories or the Actions runtime token; this would narrow what a hostile `build.rs` can reach. Until built, `T6` states the residual without it.

## 4. Check lifecycle (fixes "stuck pending")

1. `open` (Z0) finds or creates the **single** App check run for (head SHA, `assay / verdict`) and sets it `in_progress` with `external_id = pending:<run_id>`, so a check always exists once the workflow starts and two same-named check runs never exist on one SHA.
2. `decide` runs `if: always()` and treats `cancelled`, `skipped` and `failure` of its needs as explicit states, so a failed or timed-out upstream job still ends in a conclusion, **except** that an upstream `cancelled` caused by a newer run for the same head writes nothing and leaves the check `in_progress` for that run (`02 §5`).
3. `decide` publishes with compare-and-set on `check_run.id` and the previously read `external_id`; it re-fetches the PR head and writes nothing if superseded. The new run's `open` re-stamps the check for the new run.
4. A **reconcile** workflow every 6 hours (default-branch file, App token): lists `in_progress` verdict checks older than 2 × the tier p95 with no live workflow run and marks them `failure publish-error`; closes the checks of superseded head SHAs as `cancelled` and releases their leases; reaps expired semaphore leases; back-fills missing ledger records. Checks labelled `assay:llm-pending` with a requeue still scheduled are excluded; checks in `trust-path-unapproved` are `failure`, not `in_progress`, and are untouched.
5. If the workflow never starts at all (Actions outage), GitHub shows "Expected, waiting for status"; the owner's break-glass is the ruleset bypass, recorded outside assay and documented in `docs/operations/break-glass.md` (M1 deliverable).

## 5. Failure modes and effects

| ID | Failure | Detection | Effect | Handling |
|---|---|---|---|---|
| F1 | `claude` exits non-zero / malformed output | exit code, schema | Role missing | `Malformed` ⇒ retry once; otherwise degraded role (`required-role-degraded` on trust-path/exec-surface PRs, else `llm-degraded`) |
| F2 | `error_max_budget_usd` / `error_max_turns` (no structured output) | result subtype | Role produced nothing | `SpecialistBudgetExhausted`, **no retry**; the real `total_cost_usd` from the result is ledgered; degraded role visible in the check summary; caps re-sized from p90 (M2). On trust-path/exec-surface PRs: `failure (required-role-degraded)`, remedy `/assay retry` (cap ×1.5) |
| F3 | Quota exhausted / `rejected` rate-limit event | CLI error class, event status | LLM unavailable | Deferral and requeue (§1) |
| F4 | OAuth token expired/revoked | preflight | LLM unavailable | Loud failure + link to `docs/operations/token-rotation.md` (M2 deliverable); monthly canary catches early |
| F5 | CLI flag removed/renamed, or flag accepted but ineffective | `assay doctor` on the pinned CLI, `system/init` assertion, hook and fence canaries | Preflight fails | Pin in base config; upgrades are PRs that run the corpus |
| F6 | Model non-determinism flips a verdict | decision replay shows identical decision from stored inputs; fingerprint flip rate across re-runs in `assay report` | Flaky advisory blocker | LLM blockers advisory; promotion requires measured precision |
| F7 | Runner timeout / cancelled by newer push | job status | Half run | `decide always()` + supersede rule + cancelled-by-newer-run rule (§4) |
| F8 | GitHub API 5xx / secondary limit | HTTP | Publish fails | Backoff ×3, then `failure` `publish-error`; reconcile fixes stragglers |
| F9 | Gate flake (registry) | exit class | False failure | Z2b retries once on infra-class errors only; temper findings are never retried |
| F10 | Verifier rejects all / none | live reject rate (`FindingVerified`) outside 10–60% over ≥ 20 runs | Silent quality loss | Alert; confusion matrix on corpus; refuted ⇒ downgrade, not drop |
| F11 | Prompt bundle regression | corpus method (`04 §8`) | Noise/misses | Replay gate on prompt/rubric PRs |
| F12 | Persist (bundle upload) fails | step error | Replay and decide-only impossible for that run | **Trust-path and exec-surface PRs: fail closed** (`failure persist-error`); other PRs: warning, decision still published, and a later decide-only command reports `bundle-expired` and requires a re-run. A bundle older than 90 days behaves the same |
| F13 | Weigh-In journal missing | CP2 evidence rule | Hidden open items | Identity-based evidence rule (≥ 1 journal for agent code PRs); staged rollout (`04 §9`) |
| F14 | Cost spike (retry storm) | per-call cap, per-run cap, hourly cap, ledger | Quota burn | `--max-budget-usd = cap`, `--max-turns`, timeouts, ledgered real costs |
| F15 | Required plan stage disagrees with Z4 recomputation | `plan-mismatch` | Tampered/buggy plan | `failure` (loud) |
| F16 | Ledger repo unreadable or unwritable | API error | Budget unknown / record missing | Read: `admit` fails ⇒ LLM-unavailable policy. Write: warning; `reconcile` back-fills |
| F17 | Semaphore lease leaked (crashed `decide`) | `reconcile` sees an expired lease | Capacity reduced | Reaped after the 45-minute TTL |
| F18 | Artifact digest does not match the producing job's `outputs` | Z4 check | Possible tampering (or upload race) | `gate-error:artifact-digest` ⇒ `failure`; that artifact is never used |
| F19 | Bundle pointer or provenance check fails in decide-only | Z4 check | Possible forgery | Refuse; `failure gate-error:bundle-digest`; no re-decision from that bundle |

## 6. Fail-open vs fail-closed (P6)

| Component | Policy | Rationale |
|---|---|---|
| Required gate (temper; weigh-in once required) | **closed** (including skipped/timeout/invalid) | Compliance; error ≠ pass |
| Rubric/schema validation, `plan-mismatch`, artifact and bundle digests | **closed** | Garbage in cannot yield success |
| `base-not-protected` | `neutral` (the check is not required there) | A retarget never leaves a stale `success` because the single check run is overwritten |
| Publish | **closed** after retries | A stuck pending check is worse |
| LLM stage unavailable/degraded | **by class**: trust-path and exec-surface PRs (the `block_on_infra_for` classes) ⇒ deferral (`in_progress`, ≤ 3 requeues) or `required-role-degraded` failure; otherwise `success` with a prominent, event-recorded `llm-degraded` warning | The LLM stage is advisory until promoted (QA-05); gates still enforce; quota outages must not freeze the founder for hours |
| Verifier unavailable | Treated as "LLM unavailable" (above); candidate blockers are shown as *unverified majors* | No silent downgrade |
| Persist (bundle) | **closed** for trust-path/exec-surface PRs; **open** (warning) for others | Approval and replay depend on it |
| Telemetry, ledger write | **open** | Observability must not block merges |

## 7. Observability and reports

`assay report --since 7d`: PRs by tier; AEU and cache hit rate; latency p50/p95 per tier; gate failure causes; blocker/major/overall precision **with n and Wilson intervals** and reaction response rate; verifier confusion matrix (corpus) and live reject rate; injection-suspect and egress-scan hits; overrides, waivers and self-dismissals with reasons; deferrals and requeues; semaphore wait times; per-day lines per author (PR-splitting visibility, T17); token-expiry countdown; Actions minutes. A weekly scheduled workflow (`report`) posts it as an issue in the assay repo. Persistence is run bundles as artifacts (90 days) and the central ledger. A separate telemetry-branch **ingest** workflow is deferred to M5 and only if artifact retention proves insufficient (ADR-09).

## 8. Rollout (lower-bound estimates)

Estimates are **lower bounds for agent-assisted solo work** under temper's gate; they exclude waiting on decisions and on the two upstream dependencies. Realistic is 1.5–2× each. The regression pass moved scope earlier and added the central ledger, so the totals are higher than the first draft of this document (28–34 days).

| Phase | Deliverable | Exit criterion | Lower bound |
|---|---|---|---|
| M0 | Spec + adversarial review (two rounds) + owner H-decisions | No open review blocker; H-01..H-13 answered (H-08 and H-12 are required for M1, H-11 before M2) | done on merge |
| M1 | Walking skeleton, **no LLM**: workspace under temper; domain (findings, Z4 fingerprints, rubric, tiers); `open/plan/admit/gates-static/gates-lint/gates-test/decide`; **decide-only workflow** (comment, label and schedule triggers); temper adapter (Z2s `raised`/`exception`/config diff/inventory/manifests/lint inheritance, Z2b-lint with ratchet assertions); `assay-verdict` GitHub App, ruleset and `assay doctor`; digest delivery via GHCR including the warm `target/` artifact; artifact-digest scheme; central ledger and semaphore; check lifecycle + reconcile; decision replay; `assay-self` golden gate; `docs/operations/break-glass.md`; **H-08 behavioural check** (does an agent session push under a non-owner identity?). **Canary PRs** stopped as designed: a PR-added forging workflow, a PR-added workflow uploading a forged bundle artifact, an edit of the decide-only workflow followed by approval, a `build.rs` forging attempt including a forged Z2s artifact and cache pre-seeding, a base retarget (to unprotected and to another protected base), `.gitattributes` blinding (`diff`, `filter=lfs`), a title edit that must not cancel a review, a cancelled-then-superseded run, and two same-named check runs on one SHA | Required check gates assay's own PRs; every canary fails as designed | 10–13 days |
| M2 | `ClaudeHeadless` runner, preflight + init-clean assertion, hook and fence canaries, headless profile with `--restricted`, stream-json cost accounting, free-text egress scan, sanitiser, publish; **egress-block canary (`A-12`)**; `micro` and `lite` tiers advisory; calibration of lite/micro AEU, cache behaviour, `error_max_*` behaviour, flag combinations; `docs/operations/token-rotation.md` | `A-10`, `A-12`, `A-15`, `A-21` (lite/micro) measured | 5–7 days |
| M3 | `full` fan-out, coordinator, blind verifier, evidence/trigger/screens, re-review + dismissal/waive flow, quota requeue; ablation of roles; full-tier calibration (≥ 7 days wall clock for the weekly window) | QA-01..04 measured; role ablation done | 7–9 days |
| M4 | Weigh-In: CP1 (local) and the experimental CI adapter; required CP2 after `D-WI` lands | Seeded open items fail the gate when required | 3–4 days after upstream delivery |
| M5 | Corpus (≥ 30 dev/test at ship, growing), decision-replay verify at scale, shadow mode ≥ 4 weeks / ≥ 40 PRs, labelling protocol | QA-05, QA-06 intervals reported; QA-07 descriptive until the test split reaches 100 | 3 days + 4 weeks soak |
| M6 | Cutover: require `assay / verdict` on the first repo; cancel the SaaS after 2 clean weeks; promote `llm_blockers` / `llm_majors` per repo only when QA-05 is met | No P0/P1 incident (P0 = wrongly merged gate bypass; P1 = merge blocked > 1 h by infrastructure) | 1 day + 2 weeks |

Total build lower bound: **10+5+7+3+3+1 = 29 to 13+7+9+4+3+1 = 37 working days** (≈ 6–8 calendar weeks) plus the 4-week soak, **excluding** upstream work (Weigh-In `ci-check`/journal hook, temper requests) that is owned elsewhere and gates M4.

**Path B (smallest thing, 2–4 days).** A base-defined `pull_request_target` workflow running the pinned temper `baseline --check --locked --base <base_sha>` (failing on `ratchet.raised`), plus `claude-code-action@v1` with the OAuth token, a base-sourced REVIEW.md-style prompt and `--json-schema`/`--max-turns`/`--max-budget-usd`, advisory only. It delivers an early version of G1 and G3 with none of the topology above. Reviewers estimate it captures most of the value at a fraction of the effort; the case for the full design is the combination of the hardened gate, the Weigh-In stage, and replay/eval, not LLM review quality (`07 §4`).

**Kill / pivot criteria (decided now).**
- *Effort:* if M1+M2 actual exceed 1.5× their lower bound (> 22.5 days), switch to Path B for the LLM stage and keep the M1 deterministic stack.
- *Quota:* if an average `full` run consumes > 10% of one 5-hour window, or Anthropic terms exclude CI use of the subscription, fall back to the `ClaudeApi` runner and re-baseline cost.
- *Quality:* if after M5 recall is clearly worse than the pinned baseline (QA-07, once the test split can support a statement), adopt claude-code-action + REVIEW.md for LLM review and keep the deterministic stack. Failing QA-05 does not kill the project; it keeps LLM blockers advisory.
- *Reliability:* QA-08 > 3% of PRs held by infrastructure over 4 weeks ⇒ fix or pivot.
