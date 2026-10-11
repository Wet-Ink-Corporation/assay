# Regression pass and adversarial round 2

Spec under review: `docs/architecture/` v1. Result: v1.1. Date: 2026-10-10.

## 1. Method

Two fresh-context reviewers, run in parallel, neither having seen the conversation or round 1's reasoning:

- **Reviewer A, consistency and regression.** Read the review log and all seven documents and hunted for arithmetic that does not add up, internal contradictions, dangling references, and mismatches with the temper and Weigh-In source. It also checked every triage-table claim in the round-1 log. 33 numbered defects plus a list of triage rows whose claim was not fully true.
- **Reviewer B, adversarial round 2.** Attacked v1's *fixes*, with web access to the Claude Code and GitHub documentation (fetched as raw markdown; `docs.github.com` itself was blocked, so GitHub claims cite the raw `github/docs` repository). 14 numbered attacks (R2-1 to R2-14), tagged VERIFIED or REASONED, plus a flag-by-flag check of the headless profile and a list of v1 fixes it tried and failed to break.

Both reports arrive as sub-agent hand-backs; I treated them as data, re-derived the arithmetic myself, and checked the claims that mattered against source and the reviewers' citations before changing the spec. Where a claim is REASONED rather than VERIFIED it is still acted on if the cost of being wrong is a security hole and the fix is cheap, and it is listed in §5 as unconfirmed.

## 2. Headline

Round 2 found that round 1's fixes were mostly sound in intent and wrong in several mechanisms. The most serious:

1. **The decide-only workflow was forgeable and not base-defined.** `pull_request_review` and `labeled` run the PR's own workflow file, and its bundle was selected by artifact name, which any branch workflow can write. A forged bundle would have produced a *genuine* App-authored `success` (R2-1, R2-2). Redesigned (ADR-20).
2. **The Z2s results were not authenticated.** A hostile `build.rs` holds the run-scoped runtime token and can rewrite any artifact of the run. Output digests now authenticate every artifact Z4 consumes (R2-3).
3. **The "trusted" Actions cache was poisonable** and would have persisted after merge. Replaced by a digest-pinned OCI artifact (R2-4, ADR-21).
4. **A cancelled run could publish a final `failure` that made the newer run's identical-key `decide` a no-op** (R2-5). Fixed with one check run per SHA, compare-and-set, and a cancelled-by-newer-run rule.
5. **The temper adapter misused its own flags** (`--base origin/<x>` overrides `TEMPER_BASE`; the ratchet silently degrades to totals-only) and did compilation work in a zone claimed to be compilation-free (R2-6, A26, A28).
6. **The cost model was wrong in the direction that matters**: incremental runs were costed at 50% although the design re-reads the cumulative diff (A2). Corrected cost is **$1.28/PR, $282–$423/month AEU** (v1: $0.90, $197–$296). Actions minutes rose from $33–58 to **$48–80/month**. Effort rose from 28–34 to **29–37 working days**.

## 3. Report A: consistency and regression

| # | Sev | Verdict | Resolution (v1.1 location) |
|---|---|---|---|
| A1 | high | ✔ | Lite verifier is 3 × $0.08, so lite is **$0.74**, not $0.70; fixed in `01` QA-13, `04 §2.3, §3`, `06 §1-2`, log REL-4 |
| A2 | med | ✔ | Incremental runs cost 100%; factor 2.5; $1.28/PR; ranges relabelled lower bound; daily cap raised to $40 (2.1× load); `01` A-02, `06 §2`, `07` A-22. Cheaper incremental specialists recorded as lever (4) |
| A3 | low | ✔ | `meta` expected $1.70; mix understatement footnoted (≈ $0.10/PR); `04 §2.3`, `06 §2` |
| A4 | low | ✔ | Minutes recomputed: trivial 4, weighted 17.15, plus polling 480 and decide-only 1,100/1,650; `06 §3` |
| A5 | low | ✔ | QA-01 relaxed to p50 ≤ 5, p95 ≤ 8 (3.8 min before gate work) |
| A6 | low | ✔ | Shard caps stated ($6.90 / $7.80); shards sequential so ≤ 4 model processes; `01` QA-13/A-03, `04 §3` |
| A7 | high | ✔ | Tier table evaluated in stated order (`needs-split`, `trivial`, `full`, `lite`, `micro`); zero-line non-docs PRs are `micro`; `04 §2.3` |
| A8 | high | ✔ | `branches:` filter removed; Z4 publishes `neutral base-not-protected`; `01` R-01, `02 §3`, `05` T15 |
| A9 | high | ✔ | Decide-only triggers restricted to default-branch-file events; `01` R-01, `02 §1`, `05` T20, ADR-20 |
| A10 | med-high | ✔ | `plan` has no credentials; budget read moved to the new `admit` job; central ledger; the phantom `finalizer` removed; `06 §1`, `05 §4`, ADR-19 |
| A11 | med | ✔ | Two-phase `external_id` (`pending:<run_id>` → `decision_key`); `evidence_key` computed outside `open`; `02 §5` |
| A12 | med | ✔ | `pending` no longer used for approval; `failure trust-path-unapproved` + label; `03 §5` |
| A13 | med | ✔ | Deferral (requeue, ≤ 3) separated from role-cap exhaustion (`failure required-role-degraded`, `/assay retry`); `04 §5` rule 8, `06 §1` |
| A14 | med | ✔ | One authority rule (numeric id, not author/committer/pusher) used verbatim everywhere; `04 §5` |
| A15 | med | ✔ | Classes `trust-path` and `exec-surface` defined; both in the default `block_on_infra_for`; `03 §1` |
| A16 | med | ✔ | Rule 7 gated on separate `llm_majors` (default advisory) with its own promotion; micro findings excluded; `01` QA-05, `04 §5` |
| A17 | med | ✔ | Carried-finding re-verification shares the verifier cap; micro re-checks deterministically only; `04 §3, §7` |
| A18 | low-med | ✔ | `admit` builds the context pack (suppress, dismissals, prior findings) from App-authored records; duplicate-of-gate remains post-hoc with cost accepted; `04 §1, §3, §4` |
| A19 | low-med | ◐ | Persist fails closed for trust-path/exec-surface PRs, warns otherwise; QA-11 carve-out; bundle expiry returns `bundle-expired`. Not made fail-closed for all PRs (disproportionate) |
| A20 | low | ✔ | "Dropped reasons" renamed disposition codes; QA-12 reworded; P4 reworded |
| A21 | low | ✔ | Enumerations repaired: rule 5 covers no-evidence and pending-triage; rule 8 names its codes; `superseded` is an event only; `FindingVerified`, `FindingDisposed`, `StageDegraded`, `CoordinatorFinished` added; `NeutralWarning` removed; `03 §5-6` |
| A22 | low | ✔ | `--max-budget-usd = cap`; calibration phases consistent (M2 lite/micro, M3 full) |
| A23 | med | ✔ | Cross-references repaired (ledger and caps are `06 §1`, cost model `06 §2`; temper list is `04 §2.2`/`03 §2.1`; `ARC-02` removed; "temper H-16"; H-12 in the M0 exit; D-TEMPER-1/2 defined in H-09; the missing runbook is now two named M1/M2 deliverables; phases renamed M0–M6) |
| A24 | med-low | ✔ | Digest lives under `[assay]` in `plugins.toml`; declared `[[plugin.step]]` keys with per-step timeouts; run schema defines `classes`, `required_roles`, steps |
| A25 | med | ✔ | Permission matrix completed (`admit`, `requeue`, `reconcile`, canary, report, `packages: read`, `actions: read`); `05 §4` |
| A26 | med (verified) | ✔ | `--base <base_sha>`; `03 §2.1` |
| A27 | low (verified) | ✔ | Gate keys are `gate, scanner, manifests`; the others are compared as defence in depth and labelled so |
| A28 | med | ✔ | `raised` and `exception` moved to Z2s; inventory by `syn` in Z2s; Z2s redefined as no PR-controlled execution |
| A29 | low | ◐ | A-10 extended (nesting, `--restricted`, `--verbose`, `--include-hook-events`, version floor 2.1.257). Behavioural verification deferred to `assay doctor`; the reviewer's local-bundle search was suggestive only |
| A30 | med-low | ✔ | Decide-only workflow and the H-08 behavioural check moved to M1; egress canary in M2; canary list expanded; `06 §8` |
| A31 | low-med | ✔ | Owner-authored PRs have no eligible approver: approver account (H-13) or ruleset bypass, stated; `04 §5`, `07` H-06 |
| A32 | low | ✔ | All of `crates/**` are trust paths for the assay repo; `02 §7`, `05` T16 |
| A33 | med | ✔ | QA-07 is descriptive until the test split has ≥ 100 cases; `01`, `04 §8` |

Triage rows corrected in `01-adversarial-review-round-1.md`: REL-4, REL-5, REL-6, REL-9, REL-12, QUAL-1, QUAL-3, QUAL-11, SEC-5, SEC-6, SEC-7, SEC-8, SEC-12, REL-1, ARCH-4, ARCH-6, ARCH-10, ARCH-12, BYP-2, BYP-7, BYP-8, BYP-10, BYP-11.

## 4. Report B: adversarial round 2

| # | Sev | Tag | Verdict | Resolution |
|---|---|---|---|---|
| R2-1 | crit | REASONED | ✔ | Bundle trusted only via `{bundle_run_id, bundle_sha256}` in the App-authored check, after verifying workflow file, event, `head_repository` and digest; evidence reuse only by stored digest; `02 §1, §5`, `05` T11, ADR-20 |
| R2-2 | high | VERIFIED | ✔ | `issue_comment`, `pull_request_target` labeled/unlabeled and `schedule` only; approval = review + `/assay recheck`, with a 20-minute poll fallback; T19 expression-injection rule added; `05` T20 |
| R2-3 | high | REASONED | ✔ | Normative artifact rule: sha256 equals the producing job's `outputs` digest for every consumed artifact; per-job nonce names; `02 §1`, `05` T6/T11, ADR-12 |
| R2-4 | high | REASONED | ✔ | No `actions/cache` for decision-bearing builds; digest-pinned OCI warm `target/` from a trusted main job; ADR-21, `06 §3` |
| R2-5 | high | REASONED | ✔ | Single check run per (SHA, name); compare-and-set; cancelled-by-newer-run publishes nothing; `02 §5`, `06 §4`, `05` T10 |
| R2-6 | high | VERIFIED | ✔ | (a) `--base <sha>`; (b) ratchet-mode assertions and independent merge-base; (c) `raised`/`exception` in Z2s; (d) lint inheritance replicated; `03 §2.1` |
| R2-7 | high | VERIFIED/REASONED | ✔ | `git check-attr` forbids attributes instead of neutralising them; `GIT_CONFIG_NOSYSTEM`, hardened fetch, numstat `-` = maximum, rename-limit and wall-clock caps, pinned base tools on sanitised copies; `03 §2.1, §9` |
| R2-8 | high | VERIFIED | ✔ | Setting nested under `permissions` (CLI ≥ 2.1.257); `--restricted`; `--verbose`; hook canary because `init` has no hooks field; `rate_limit_event` keyed on `rejected`/`allowed_warning`; no utilisation pause; prompt-cache claim narrowed; fence canary; `03 §8`, `06 §1-2` |
| R2-9 | high | REASONED | ✔ | Free-text egress scan in Z3 and Z4 (token, encodings, unexplained long runs); fail job and flag rotation; `03 §3`, `05` T2 |
| R2-10 | high | REASONED/VERIFIED | ◐ | Adopted: numeric ids, GitHub-attributed authorship, recorded push sender and clock, clean merge-from-base exclusion, scoped waivers, approver account. **Not resolved:** whether Claude Code cloud sessions can push under a non-owner identity (H-08), so P10 may be only a convention; `04 §5`, `01` R-08, ADR-16/22 |
| R2-11 | med-high | VERIFIED (spec text) | ✔ | Z2b split into `gates-lint`/`gates-test`; explicit trust-free docs allowlist excluding executable agent instructions and `include_*!` targets; precise exec-surface detection; direction-aware baseline rule; renames matched on both sides; `04 §2`, `03 §2.1`, ADR-12 |
| R2-12 | med | VERIFIED | ✔ | `edited` gated on `changes.base`; `cancel-in-progress` only for `synchronize`; pipeline `decide` has no shared group; decide-only group non-cancelling and idempotent; event-loop guards; `02 §5`, `05` T12 |
| R2-13 | med | VERIFIED | ✔ | Blocking set from triaged fields, `pending-triage` for untriaged; one ack bound to a blocking-set digest; ≥ 1 journal for agent code PRs; event-level append-only check; `04 §9` |
| R2-14 | med | VERIFIED/REASONED | ◐ | Central ledger and ref semaphore (ADR-19); reservations only for runs that spend; superseded checks closed; `--max-budget-usd = cap`; real cost ledgered from `error_max_*` results. The reviewer's "kill on own accounting" idea is dropped because cost exists only in the final result; `06 §1` |

V1 fixes the reviewer tried to break and could not: the dedicated-App check (cannot be pre-seeded; the ruleset `integration_id` pins the source), head re-fetch in `decide`, the Z2s/Z2b split and `plan-mismatch` recomputation (weak only in artifact authentication, now fixed), the temper findings contract (`raised`, `exception`, `--findings-out` path restriction, `--locked`, exit 0 on raise), the tiering arithmetic, GitHub's own self-approval block, and the egress-plus-no-Bash-plus-empty-cwd layering for the network path.

## 5. Not resolved, or unconfirmed (carried into the spec as labelled assumptions)

- **H-08 feasibility** (agent sessions push under a non-owner identity). Behavioural check scheduled in M1; until it passes, P10 is a convention (`07` A-17).
- **Same-name check runs** on one SHA are avoided by design rather than relied on (`A-27`); the canary still tests the platform behaviour.
- **GitHub trigger semantics** (`pull_request_review`/`labeled` run the merge-commit file; `issue_comment` and `schedule` run the default branch) come from the round-2 reviewer's reading of `github/docs` (`A-28`); M1 canaries prove them on the real repo.
- **Warm `target/` reuse** across checkouts: cargo fingerprints can depend on the absolute workspace path and the toolchain, so reuse may be partial (registry dependencies reuse well, workspace crates do not). The design cleans workspace crates anyway; the latency benefit is `A-29` and is measured in M1.
- **Flags** `--restricted`, nested `permissions.blockReadsOutsideWorkingDirectories` and `autoContinueAtUsageLimit` are documented per the round-2 reviewer but were not found in the author's locally installed CLI bundle; `assay doctor` verifies them behaviourally (`A-10`).
- **Prompt-cache sharing** is now claimed only for same-model, same-schema specialists; its size is a measurement (`A-15`).
- **Third review of the delta.** v1.1 adds structure that no reviewer has attacked yet (the `admit` job, the central ledger and semaphore, the decide-only command set, the approver model). The previous two rounds each found a double-digit number of defects in the preceding revision, so assume v1.1 has some. Recommended before any build starts: a third, narrower review of exactly those additions.

## 6. Counts

Report A: 33 defects, 31 ✔, 2 ◐ (A19, A29), 0 rejected. Report B: 14 attacks, 12 ✔, 2 ◐ (R2-10, R2-14), 0 rejected. Combined with round 1: 62 + 33 + 14 = **109 findings across three review passes**, none rejected outright.
