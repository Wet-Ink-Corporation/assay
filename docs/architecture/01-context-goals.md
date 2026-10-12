# 01 · Context, goals and requirements

Status: **v1.2 (after adversarial review rounds 1–3 and the regression pass, see `docs/review/`)**. Every number here is a decision or a labelled assumption (`A-nn`, register in `07`). Requirement IDs (`R-nn`, `QA-nn`) are cited by the other documents and by the review log.

## 1. What assay is

assay is a pull-request review orchestrator for Wet Ink repositories. For every PR it (1) runs deterministic gates contributed by temper and Weigh-In (and repo-specific exec gates), (2) runs a tiered fan-out of read-only Claude Code headless reviewers, (3) maps all findings through a deterministic decision script to one required status check published by a dedicated GitHub App, and (4) keeps an auditable record of every run so the *decision* can be replayed and the pipeline measured and tuned.

It replaces a paid review SaaS (Greptile/CodeRabbit) for the owner's own repositories. It does not replace human review of one-way decisions; it makes sure those decisions are surfaced and not skipped.

**Honest scope of the assurance** (changed by review): the deterministic gates give strong assurance against careless authors and against steered or sloppy agent sessions that *edit configuration and code*, because assay runs pinned tooling from the base branch and refuses weakened gate configuration. They give **only honest-mistake assurance against an author who executes hostile code at build/test time** (a malicious `build.rs`, proc-macro or test), because any CI that compiles PR code is exposed to that. assay compensates by treating the exec surface (build scripts, proc-macros, dependencies) as owner-approved territory (`R-07`) rather than by pretending the gate output is unforgeable.

## 2. Actors

| Actor | Interest | Trust |
|---|---|---|
| Owner (Ryan) | Merge quickly with compliance; no blocking questions shipped | Trusted; holds break-glass, approval and dismissal authority, but only through a **privileged identity** (P10): an account never authenticated in any agent-reachable context (no Claude GitHub App authorisation, no `gh` token on a machine where agents run, no PAT given to a session) that is not the PR author or a pusher of the head. The privileged identities are the dedicated approver account (`H-13`, mandatory for M1) and an optional separate break-glass account. The owner's daily account, which is connected to Claude Code, is a **maintainer** (`.assay/owners` `maintainers`): it may author and push but takes no privileged action (R3-01, accepted by the owner 2026-10-11, `07 §3`) |
| Human author | Fast, accurate feedback | Trusted to author, **not** to authorise their own gate changes |
| Agent author (Claude Code cloud/desktop sessions) | Same | Untrusted: its output, its PR text and its claims about itself are data. `H-08` has two parts: (a) agents commit and push under an identity **distinct from the owner's**; (b) no agent-reachable credential can comment, review, label or merge as any privileged identity. A session holding the owner's GitHub authorisation can comment, approve and merge as the owner, so (a) alone does not protect owner-only actions. Both parts are checked behaviourally in M1 (`06 §8`); until they are, P10 is a convention, not a control |
| Fork / external contributor | Out of scope for v1 (private repos); design must fail safe if one appears | Untrusted |
| GitHub | Hosts events, runners, checks | Trusted platform; token scopes limit blast radius |
| Anthropic | Model, auth, quota | Trusted vendor; terms and quota mechanics are an external risk (`A-01`, `A-13`) |

## 3. Goals

- **G1** Block merges that violate temper's rust-craft gate, trusting nothing produced on the author's machine and nothing in the PR that defines the gate (workflow, tooling pin, config). Assurance level as stated in §1.
- **G2** Block merges that carry unresolved blocking Weigh-In items. **Staged**: advisory until Weigh-In ships the contract in `04 §9` (`D-WI`), required afterwards.
- **G3** Give a high-precision LLM review (bugs, security, design, tests, docs) at a cost the Max subscription absorbs. **LLM blockers start advisory** and become merge-blocking per repo only after measured precision clears the promotion rule in `QA-05`.
- **G4** Make every *decision* reproducible and explainable from stored inputs (decision replay).
- **G5** Keep the plugin boundary clean: temper and Weigh-In are consumed through CLI/JSON contracts only. v1 ships two built-in adapters plus a generic `exec` adapter; a general third-party plugin ecosystem is not a v1 goal.
- **G6** Dogfood: assay is Rust, built under temper, decided through Weigh-In.
- **G7** Bake in practice from Cloudflare (tiers, coordinator, "what not to flag", circuit breakers, bias to approve), Anthropic (REVIEW.md, structured severities, verification pass; managed review's neutral check) and the community (verifier-drop logging, deterministic adjudication).

## 4. Non-goals

- **NG1** Auto-fixing or pushing commits. assay never writes to a PR branch (including via suggestion blocks).
- **NG2** Replacing CODEOWNERS/branch protection. assay is one required check, but it **requires** specific ruleset settings as prerequisites (`05 §7`, enforced by `assay doctor`).
- **NG3** Review of third-party/client repositories on the subscription (`A-01`); the `ModelRunner` port exists so an API-key adapter can serve them.
- **NG4** A web UI. Check run, PR comments and `assay report` are the interface.
- **NG5** Cross-PR/whole-repo semantic indexing in v1. Reviewers use Read/Grep on the head tree.
- **NG6** Multi-tenant hosting; merge queues (`merge_group`) in v1; stacked PRs in v1.
- **NG7** Detecting a PR that fakes test success from inside compiled code (see §1).

## 5. Design principles

| ID | Principle | Consequence |
|---|---|---|
| P1 | **Deterministic code decides; models advise.** | Merge conclusion comes from `decide` (pure function). Models cannot approve. LLM output cannot lower a gate finding. |
| P2 | **Trust root is the protected base, pinned by SHA.** | The workflow file comes from the default branch (on github.com `pull_request_target` runs the default branch's file whatever the PR's base, `A-28`); assay version, plugin pins, gate config, REVIEW.md rules and prompts come from the PR's base commit; nothing comes from the head. All pins are commit SHAs or digests, never tags. Every protected base pins the same assay digest as the default branch (doctor, `05 §7`). |
| P3 | **No secret meets untrusted code or untrusted text together with a write path.** | Job classes with disjoint capabilities (`02 §3`). |
| P4 | **Findings are claims; claims need evidence and a trigger.** | Every finding carries a quoted span located in the head (or base, for deletions) tree and, from `major` up, a concrete triggering input or path; otherwise dropped, or capped at `minor` where `04 §4` says so, with a logged disposition code. |
| P5 | **Bias to approve; block only on what can be verified.** | Gate failures block. LLM `blocker`s and `major`s are advisory until promoted per repo (`QA-05`, `llm_blockers`, `llm_majors`). |
| P6 | **Fail-closed where deterministic, explicit where not.** | Gate errors and skipped required gates fail the check. LLM infrastructure errors follow a visible, per-repo policy. |
| P7 | **Every run leaves an auditable record.** | Run-input bundles and a per-job event stream (not a distributed event store) support decision replay, metrics and eval. |
| P8 | **Plugins are consumed through contracts.** | assay never links a sibling's internal crates; temper/Weigh-In knowledge lives in two named adapters that parse their documented JSON. Missing contract surface is requested upstream (`07 §3`). |
| P9 | **Local is a shift-left convenience, never a trust anchor.** | Local verdicts are never accepted by CI. |
| P10 | **Identity separation is a control.** | Owner-only actions (approve trust paths, override, waive, dismiss blockers, retry, Weigh-In ack) must come from a numeric GitHub user id in `.assay/owners` ∪ `approvers`, which list privileged identities only (accounts never authenticated in any agent-reachable context, §2), and that id must not be the PR author, a pusher or a committer of the head (clean merge-from-base commits excepted). Comment-borne actions count only as App-recorded command records, never as comment text read later (`04 §5`). One rule everywhere (`04 §5`). |

## 6. Functional requirements

| ID | Requirement |
|---|---|
| R-01 | Workflow triggers, all from the default-branch workflow file (`pull_request_target`, no `branches:` filter, so a retarget to a non-protected base is decided too and yields `failure base-not-protected`, which is fail-safe because the check is not required there): `opened`, `synchronize`, `reopened`, `ready_for_review`, and `edited` only when `changes.base` is present (title and body edits never enter the pipeline's concurrency group, `02 §5`). A second *decide-only* workflow re-decides from stored inputs and is triggered only by events that run the **default-branch** workflow file: `issue_comment` (`created`, PR comments only, the closed command grammar of `04 §6`: `/assay recheck`, `waive`, `override`, `dismiss`, `retry`, `weigh-in-ack`) and `pull_request_target` `labeled`/`unlabeled`; an hourly `schedule` poll covers PRs in `pending-approval`. `pull_request_review` is not a trigger, because that event runs the workflow file from the PR merge commit. The verdict check is one App-authored check run per (head SHA, name), published by the `assay-verdict` GitHub App: `assay / verdict` for the default branch and `assay / verdict @<base>` for each other protected base. It is created `in_progress` at the start of every run and updated only under the per-head publish lock in the ledger (`02 §5`, ADR-23). |
| R-02 | Each gate runs without secrets and write permissions and reports `assay.findings/1`. A gate crash, timeout, missing/invalid output or a **skipped required gate** is `failure`. |
| R-03 | The LLM review runs only if the plan tiers the PR above `trivial`, the budget ledger allows it, the PR is not a draft, and auth preflight (inside the `review` job) proves `authMethod == oauth_token` and a clean session init (no MCP server; tools exactly `Glob`, `Grep`, `Read`; every plugin, skill and agent a built-in of the pinned CLI on the allowlist pinned next to `claude_cli`, since built-in mods cannot be switched off headlessly; hooks are excluded structurally by `--restricted` and proven absent by canary; mods and AGENTS.md by the mod and AGENTS.md canaries, `03 §8`). |
| R-04 | `decide` is a pure function of validated, persisted inputs and a versioned rubric. Anything failing schema validation is rejected. |
| R-05 | Inline comments are posted from validated findings only, capped at 8 per run, deduplicated by a Z4-computed fingerprint, never re-posted after a *valid* dismissal (`04 §6`). |
| R-06 | Re-review is incremental in what the model is asked to find, never in what the verifier re-judges: carried-over findings are re-verified every run within the tier's verifier cap (`04 §7`; micro re-checks them deterministically only). |
| R-07 | **Trust paths and exec surface** (`04 §2.2`) force tier `full`, add the `meta` reviewer, and require an approval review on the head SHA from an owner or approver login that satisfies P10. |
| R-08 | Waivers and break-glass: (a) `/assay waive <finding-id>` by a P10-eligible owner waives one *human-decision* gate finding (raised baseline, `ask` exception, gate-config drift, test-inventory decrease, manifest switch) and nothing else; clippy, build and test failures are never waivable; (b) override is a P10-eligible owner's `/assay override llm|all <reasons_digest> <reason>`, bound to the digest of the current reason codes printed in the summary and void if a reason is added; `override llm` neutralises only rule 8 (LLM stage), `override all` is the whole-verdict break-glass; the verdict becomes `neutral`, is recorded and listed in the weekly report (the two scopes separately). |
| R-09 | `assay replay --verify <run>` recomputes the *decision* from the persisted input bundle and compares it with the recorded decision. Model-level replay uses recorded model outputs only. |
| R-10 | `assay check --local` runs the pipeline against the working tree for pre-push use; advisory output only. |
| R-11 | Weigh-In integration: local pre-push checkpoint (CP1) and CI evidence gate (CP2), `04 §9`. |
| R-12 | Feedback (reactions, `/assay dismiss`, resolved threads, "fix commit touched the anchor") is captured as events and joined to findings for eval. |

## 7. Quality attributes (targets)

Latencies are wall clock from event to check conclusion. All specialists of a run execute concurrently. Gate (Z2b) time dominates `.rs` PRs and assumes a warm `target/` delivered as a digest-pinned OCI artifact built by a trusted main-branch job (`06 §3`).

| ID | Attribute | Target |
|---|---|---|
| QA-01 | Latency, `trivial` (docs allowlist only) | p50 ≤ 5 min, p95 ≤ 8 min (runner start 80 s + open 0.5 + plan 1.0 + decide 1.0 is already 3.8 min, `06 §3`) |
| QA-02 | Latency, `micro` | p50 ≤ 8 min, p95 ≤ 15 min |
| QA-03 | Latency, `lite` | p50 ≤ 10 min, p95 ≤ 18 min |
| QA-04 | Latency, `full` | p50 ≤ 15 min, p95 ≤ 25 min |
| QA-05 | **Blocker promotion rule** | LLM blockers become merge-blocking for a repo only when the lower 90% Wilson bound of blocker precision is ≥ 70% over ≥ 20 labelled blockers accumulated (not a rolling 30 PRs). Demote if the upper bound falls below 80% over the same accumulation. `llm_majors` is promoted by the same rule applied to labelled majors, separately from blockers |
| QA-06 | Overall comment precision | point ≥ 70% and lower 90% bound ≥ 60% over ≥ 100 labelled comments; report n and interval in `assay report` |
| QA-07 | Recall | Descriptive report only (point estimate, n, interval) until the held-out *test* split has ≥ 100 cases, because at 10 cases one case is 10 points and no non-inferiority margin is testable. From ≥ 100 cases, paired across k = 3 runs per case: lower 90% bound of (assay − pinned baseline, `A-23`) recall ≥ −10 points. Seeded-mutation recall is a regression smoke test, not evidence for QA-07 |
| QA-08 | Infra-caused merge delay | ≤ 1% of PRs held by assay infrastructure; break-glass available and recorded; the decide-only workflow re-decides within 2 min of a `/assay` comment or label event, and within about 60 min via the hourly off-peak `schedule` poll after a bare approval review (best effort: GitHub may delay or drop scheduled runs; the `pull_request_review` → `workflow_run` nudge would bring it to ≈ 2 min once `A-32` is verified) |
| QA-09 | Noise | ≤ 8 inline comments and exactly 1 summary per run |
| QA-10 | Determinism | `decide(inputs)` is byte-identical for identical inputs (property-tested; golden corpus) |
| QA-11 | Auditability | 100% of conclusions whose run-input bundle persisted and is inside the 90-day window are reproducible by `assay replay --verify`, including decide-only conclusions, which persist their own decision bundle chained to the evidence bundle (`02 §5`). Persist failure (F12) fails closed for trust-path and exec-surface PRs and is visible for others; an expired or deleted bundle makes decide-only return `bundle-expired` and require a re-run (F20) |
| QA-12 | Disposition visibility | 100% of findings that are dropped, capped, moved or downgraded carry a disposition code and a `FindingDisposed` event; the verifier's confusion matrix on the corpus and its live reject rate (alert outside 10–60% over ≥ 20 runs) are reported |
| QA-13 | Cost per run (API-equivalent, **AEU**) | trivial $0; micro cap $0.25 (expected $0.15); lite cap $0.74 (expected $0.50: $0.50 specialists + 3 × $0.08 verifier = $0.74); full cap $2.30 (expected $1.50), full + `meta` cap $2.60 (expected $1.70). A sharded full run (`04 §3`, ≤ 3 shards, run sequentially) scales the cap by shard count: $6.90, $7.80 with `meta`. Caps are enforced by the CLI flag `--max-budget-usd = cap` plus assay's own ledger of the real cost the CLI reports, including on `error_max_*` results (`06 §1`); sized provisionally (`A-21`) |

## 8. Scale assumptions

Solo founder, private repos, agent-heavy authorship.

- **A-02** 10 PRs/day typical (5–15 range), **2.5 runs per PR** (agents push repeatedly; `A-22`). Tier mix per PR: 20% trivial, 25% micro, 35% lite, 20% full. Specialists always review the full cumulative diff (`04 §7`), so an incremental run costs 100% of the tier cost; only carried-finding verification is incremental. Expected AEU per PR = (0.25×0.15 + 0.35×0.50 + 0.20×1.50) × (1 + 1.5×1.0) = 0.5125 × 2.5 = **$1.28**. Per day: **$12.8** at 10 PRs, **$19.2** at 15 PRs; per month (22 days): **$282–$423 AEU**. This is a lower bound on cost: the mix prices every `full` PR without `meta` (expected $1.70 for `meta` PRs, so trust-path PRs add up to ≈ $0.10/PR averaged) and ignores shards. On a subscription this is quota, not cash.
- **A-03** Concurrency: 2 model-running `review` jobs at once across *all* repos on the subscription, enforced by a central semaphore in the assay ledger (GitHub Actions concurrency groups are repository-scoped and cannot do this; `ADR-19`). Within a run, roles run concurrently (≤ 4 model processes) and shards run sequentially. The daily and rolling-window guardrails are per subscription (`06 §1`).
- **A-04** Typical filtered diff sizes as in `04 §2`; to be replaced by measurement from `gh pr` history in M1.
