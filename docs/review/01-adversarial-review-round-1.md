# Adversarial review, round 1

Spec under review: `docs/architecture/` v0. Result: v1. Date: 2026-10-10. Rows below were corrected after the regression pass (`02-regression-pass.md`): corrections are marked **v1.1**.

## 1. Method

Five independent reviewers, each a fresh-context agent that had not seen the spec or the conversation, ran in parallel with read access to the spec and to the temper and Weigh-In source, and web access to official documentation. Each had one lens and was told to break the design, not to be polite, and to tag each finding VERIFIED (confirmed against source or docs) or REASONED (inference).

| Lens | Prefix | Findings |
|---|---|---|
| Security, prompt injection, token/permission blast radius | SEC | 12 |
| Reliability, cost, quota, latency, idempotency | REL | 12 |
| Review quality, evaluation validity, LLM blind spots | QUAL | 12 |
| Architecture, scope, coupling, build-vs-buy | ARCH | 12 |
| Adversarial author: bypass the temper and Weigh-In gates | BYP | 14 |
| **Total** | | **62** |

Four of the five independently found the same top problems (forgeable required check; Z2 executing PR code; the unusable `--allowedTools` isolation; the idempotency/trigger design that cannot recover). That convergence is the strongest signal in the round.

## 2. Triage rules

A finding is **accepted** (✔) when it names a concrete failure the spec as written permits and the fix is proportionate; **modified** (◐) when the problem is real but the proposed fix is changed, narrowed or staged; **deferred** (⏸) when it is real but out of v1 scope (recorded as an open question or phase item); **rejected** (✘) when it is wrong or disproportionate. Severity is the reviewer's, except where I re-rated and say so. Nothing was rejected outright; four were modified for stated reasons and four partly deferred.

## 3. What I verified myself before triage

Checked directly against sibling source (not relayed):

- `baseline --check` computes `raised` but `ok = errors==0 && newCode==0 && increases==0`; exit is `ok ? 0 : 1`. A raised baseline exits 0 (`temper/scripts/lib/baseline.mjs:445`).
- `temper gate` takes only `--no-cache`, `--check`, `--json`; its base comes from `sessionBase` (TEMPER_BASE or HEAD); it has no `--base/--locked/--findings-out` (`temper/scripts/temper.mjs:112-117`).
- Lint exceptions (`ask` hits) are listed but do not fail the gate (`temper/scripts/lib/gate.mjs:243-244, 358-363`).
- Weigh-In's journal hook is repo-local and runs only when `CLAUDE_CODE_REMOTE=true` (`weigh-in/.claude/hooks/weigh-in-journal.sh:8`); the plugin's `hooks.json` has no journal hook; v0.6.0 was released 2026-10-10 (`CHANGELOG.md`), so the v0 pin of "0.5.4" was stale.

## 4. Correction carried from earlier in the conversation

In the first answer I said CI could reuse a locally produced approving temper verdict via its fingerprint. That was wrong: temper stores verdicts in local `.git/temper/` state that is never pushed, so a verdict cannot be a trust anchor for CI. The spec's P9/ADR-06/ADR-10 encode the correct position (local output is advisory; CI runs its own gate).

## 5. Claims I could not verify (carried into the spec as labelled assumptions)

- Cloudflare's per-tier costs ($0.20 / $0.67 / $1.68) were used as reference points; the reviewer could not find them on the fetched post. They are reference-only (`QA-13` no longer cites them).
- A Kodus "verifier dropped true bugs" lesson: no primary source found. The behaviour (downgrade rather than drop; confusion matrix) is adopted on its own merits.
- The announced Agent SDK / headless-usage credit change: secondary sources only (`A-01`).
- Egress block mode on GitHub-hosted runners (`A-12`); GHCR cross-repo access with `GITHUB_TOKEN` (`A-20`); SaaS prices (third-party, `A-19`); the exact fields of the `rate_limit` event; what `claude -p` emits when it hits `--max-budget-usd` (docs say no structured output; confirmed in P2 test); the Weigh-In atom field name `weighin_item` (`A-11`).

## 6. Triage table

| ID | Sev | Verdict | Resolution (v1 location) |
|---|---|---|---|
| SEC-1 | crit | ✔ | Dedicated `assay-verdict` GitHub App; ruleset pins its id; idempotency matches `check_run.app.id`; ADR-11, `02 §3`, `05` T7 |
| SEC-2 | crit | ✔ | Empty cwd, `--add-dir`, `--safe-mode`, `disableAllHooks`, clean-init assertion, prompts as files; `03 §8`, `05` T2 |
| SEC-3 | crit | ✔ | No Bash (`--tools Read,Grep,Glob`); `/proc` etc. denied; env scrub; mandatory egress block; `--max-budget-usd` demoted to backstop; `03 §8`, `05` T2, `06 §1` |
| SEC-4 | high | ✔ | `run.json` untrusted/recomputed; random artifact names; job `outputs` digests; no attest in Z2; `02 §4`, `05` T11 |
| SEC-5 | high | ✔ | Honest G1 scope (`01 §1`); exec surface owner-approved (`04 §2.2`); Z2s/Z2b split (ADR-12). **v1.1:** v1's Z2s still ran compiling steps (`nextest list`, the temper `baseline`); Z2s now means no PR-controlled execution, `raised`/`exception` moved into it, tests moved to a separate VM; `05` T6 |
| SEC-6 | high | ✔ | Exclusions from base globs only; `.rs` never excluded; lockfile ⇒ lite + deny; docs allowlist; `04 §2.1, 2.3`, T17. **v1.1:** v1's first-match table put `trivial` last, so the allowlist was unimplementable, and the allowlist admitted executable agent instructions; `trivial` is now evaluated second with an explicit allowlist |
| SEC-7 | high | ✔ | P10/ADR-16 identity separation; extra triggers; `04 §6`, `05` T9. **v1.1:** v1 stated the authority rule in two incompatible forms ("owner ≠ author" vs "not author or pusher") and "pusher" was not computable; one numeric-id rule with a recorded push sender and clock (`04 §5`) |
| SEC-8 | med-high | ✔ | No downgrade on deferral; `pending` instead of `neutral`; policy by class; `03 §5`, `06 §6`. **v1.1:** the class vocabulary (`trust-path`, `exec-surface`) is now defined and both are in the default `block_on_infra_for`; role-cap exhaustion is separated from deferral so it cannot livelock |
| SEC-9 | med | ✔ | Owner-corroborated decisions; `04 §9` |
| SEC-10 | med | ✔ | `suggestion` fences stripped; fp computed in Z4; `03 §3`, `04 §6` |
| SEC-11 | med | ✔ | Template hygiene list; `05` T19 |
| SEC-12 | low-med | ✔ | `protected_bases`, immutable SHA checkout; `05` T15/T19. **v1.1:** v1's `branches:` trigger filter defeated `base-not-protected` and the retarget defence and is removed; no cache is used for decision-bearing builds |
| REL-1 | crit | ✔ | evidence_key vs decision_key; decide-only workflow; `02 §5`, `01` R-01. **v1.1:** v1's decide-only triggers (`pull_request_review`, `labeled`) ran the PR's own workflow file and its bundle was selected by name; now default-branch triggers only and a digest pointer in the App-authored check (ADR-20) |
| REL-2 | high | ◐ | `open` job creates the check; `decide always()`; supersede rule; reconcile; `06 §4`. `merge_group` declared unsupported (NG6) rather than built |
| REL-3 | high | ✔ | Targets restated (QA-01..04); gate findings removed from reviewer context; concurrent roles; trusted cache; `06 §3` |
| REL-4 | high | ✔ | Distinct non-retried budget outcomes; `03 §7`, `04 §3`, `06 §1-2`, F2. **v1.1:** the arithmetic claim was false for lite: 0.50 + 3 × 0.08 = **$0.74**, not $0.70 (full $2.30, full+meta $2.60 and micro $0.25 hold). "Own accounting kills at the cap" was not implementable (`total_cost_usd` exists only in the final result; SIGTERM loses it), so the hard stops are `--max-budget-usd = cap`, turns and wall clock, and real costs are ledgered |
| REL-5 | high | ✔ | Cost counted per run; no tier degrade for trust paths; drafts skip LLM; `01 §8`, `06 §1-2`. **v1.1:** v1's 1.75× per PR assumed incremental runs cost 50%, which the design contradicted (specialists re-read the cumulative diff): the factor is **2.5**, $1.28/PR, $282–$423/month AEU. The "synchronous GitHub cost ledger" over per-commit check runs was unlistable; replaced by the central ledger (ADR-19) |
| REL-6 | high | ◐ | Two series + pause on rate-limit events; calibration moved to M3 (v1.1: the utilisation-based pause is removed, a `setup-token` cannot read utilisation, and only `rejected`/`allowed_warning` events trigger it); kill threshold set at 10% of a window (reviewer suggested ~5%; unmeasured, revisit P3); A-01 credit-change tripwire; `06 §1, §8` |
| REL-7 | high | ✔ | Defined deferral state machine, event-driven requeue + 2-hour dispatch, class-based default; `06 §1, §6` |
| REL-8 | med | ✔ | Required roles per tier; verifier outage = LLM unavailable; `04 §3`, `06 §6` |
| REL-9 | med | ✔ | Polling replaced; `06 §3`. **v1.1:** v1's $33–58/month excluded polling, decide-only runs and the 4-job minimum for trivial runs; restated as **$48–80/month** |
| REL-10 | med | ✔ | Fixed system prompt, shared cwd, `--exclude-dynamic-system-prompt-sections`; `06 §2` |
| REL-11 | med | ✔ | Base pinned by SHA; fallback chain; range-diff; `02 §5`, `04 §7` |
| REL-12 | med | ✔ | Estimates re-derived, phases add missing deliverables, calibration reordered, replay budget separate; `06 §8`. **v1.1:** 29–37 working days after moving the decide-only workflow and the central ledger into M1; phases renamed M0–M6 to stop colliding with principles P1–P10 |
| QUAL-1 | crit | ◐ | LLM blockers advisory until QA-05; blind per-candidate verifier; refuted ⇒ downgrade not drop; confusion matrix. An *executable witness* for blockers is deferred (v1.1: now actually listed in `07 §5`) |
| QUAL-2 | high | ◐ | Blind roles; verifier on a different tier; self-review recall reported separately. Second model family and `cargo-mutants` deferred (open questions) |
| QUAL-3 | high | ✔ | QA-05, QA-06 restated as intervals over accumulated labelled data. **v1.1:** QA-07 as a "within 10 points" non-inferiority claim could not be supported by a 10-case test split (one case = 10 points); it is a descriptive report until the test split reaches 100 cases |
| QUAL-4 | high | ✔ | Dev/test split, deterministic scorer, k=3, flip-based acceptance, off-peak separate ledger; `04 §8` |
| QUAL-5 | med-high | ✔ | Blind stratified labelling, shadow visibility defined, anchor-changed label, response rate reported; `04 §6, §8` |
| QUAL-6 | high | ✔ | Whole-file search and relocation, base-side evidence, shared normaliser, `omission` kind; `03 §3`, `04 §4` |
| QUAL-7 | high | ✔ | `introduced_by: interaction`, caller citation, cluster sharding, move-not-drop; `04 §3-4` |
| QUAL-8 | high | ✔ | Cumulative target + marked increment; carried findings re-verified; anchor-gone ≠ resolved; `04 §7` |
| QUAL-9 | high | ✔ | Self-dismiss ignored; groups by root cause; operational severity definitions; `04 §3, §5-6` |
| QUAL-10 | med | ✔ | Fingerprint computed only in Z4 from closed `class`, symbol, evidence, occurrence; fuzzy dismissal matching; `03 §3` |
| QUAL-11 | med | ✔ | Gate findings out of reviewer context + Z4 dedupe; ablation in M3; the Rust-specific guidance is folded into the `design` prompt bundle (v1.1: there is no separate `rust` role), no shell; trigger required; extended not-flag list; `04 §1, §3` |
| QUAL-12 | med | ✔ | `micro` tier; degraded-role visibility; shard cost scaling; `04 §2.3, §3` |
| ARCH-1 | high | ✔ | Weigh-In CP2 rebuilt and staged; `D-WI`; ADR-17; `04 §9` |
| ARCH-2 | high | ✔ | Temper adapter rewritten around `baseline --check`; exit code not trusted alone; `03 §2.1` |
| ARCH-3 | high | ✔ | P8/G5 restated: two named adapters, no `weighin-core` link; `01`, ADR-01 |
| ARCH-4 | high | ✔ | Decide-only workflow, key split; self-approval resolved by H-08; `01` R-01, `02 §5`. **v1.1:** v1 set `external_id = evidence_key` at `open`, which has no `contents` permission and cannot compute it; two-phase `external_id` (`pending:<run_id>` then `decision_key`) |
| ARCH-5 | high | ✔ | Build-vs-buy rewritten with economics, Path B and effort kill criterion; ADR-18, `07 §4`, `06 §8` |
| ARCH-6 | med-high | ✔ | Decision replay over a persisted bundle; audit stream not event-sourced aggregate; ingest deferred; ADR-09/15. **v1.1:** QA-11 now carves out persist failure (fail closed for trust-path/exec-surface PRs) and bundle expiry |
| ARCH-7 | med | ✔ | Stage graph reconciled (validation twice; no gate findings in reviewer context; concurrency 2×4) |
| ARCH-8 | high | ◐ | Digest-pinned OCI delivery; attestations not assumed (A-08); `A-20` flagged unverified; ADR-14 |
| ARCH-9 | med | ✔ | Profile decided in `03 §8` |
| ARCH-10 | med | ◐ | Duplication of `rust.yml` kept **deliberately** (a PR can edit `rust.yml`, so its result is untrusted) but made affordable with a warm `target/` (v1.1: a digest-pinned OCI artifact, not the Actions cache, which can be pre-seeded); reviewer's alternative (make `rust.yml` the check) rejected for that reason; `06 §3` |
| ARCH-11 | med | ✔ | Kill criteria extended (effort, QA-08, incident definitions); `06 §8` |
| ARCH-12 | low | ✔ | Enums, keys, preflight location, numbers, watchdog repaired in v1. **v1.1:** "all repaired" was overstated; the regression pass found 33 further consistency defects (classes, disposition codes, event kinds, manifest keys, cross-references, permission matrix), now repaired |
| BYP-1 | crit | ✔ | = SEC-1 |
| BYP-2 | crit | ◐ | Cannot be fully prevented for compiled code; honest scope, exec surface owner-approved, static/build split, recompute in Z4; `01 §1`, `05` T6. **v1.1:** v1 cited a mitigation (separate uid/container) that was not specified anywhere; it is now an explicit M2 stretch, not a claim; tests moved to a separate VM and Z2s results are authenticated by output digests |
| BYP-3 | high | ✔ | Corroboration, append-only journal, `04 §9` |
| BYP-4 | high | ✔ | Blocking set independent of the raiser; `blocks` unknown ≠ none; identity-based evidence rule; `04 §9` |
| BYP-5 | high | ✔ | Exceptions fail unless allowlisted; test inventory; manifest switches; `03 §2.1` |
| BYP-6 | high | ✔ | Trust-path superset; skipped required gate = error; lints-table diff; `04 §2.2`, `03 §1-2` |
| BYP-7 | high | ✔ | `edited` (with `changes.base`) trigger, `protected_bases`, `base_sha` in keys; `05` T15. **v1.1:** the `branches:` filter is removed (see SEC-12) |
| BYP-8 | high | ✔ | = SEC-7 plus push-clock-ordered override (v1.1: the "timeline events" ordering had no API source for ordinary pushes); `04 §5-6` |
| BYP-9 | high | ✔ | Trust-path list widened; pins by SHA/digest; `04 §2.2`, `05` T14 |
| BYP-10 | med | ✔ | explicit base; `ratchet.raised` parsed; `03 §2.1`. **v1.1:** v1's `.gitattributes` neutralisation did not work (the in-tree file still applies, and LFS filters are registered system-wide): `git check-attr` now forbids the attributes, with `GIT_CONFIG_NOSYSTEM` and hardened git (`03 §9`); `--base` is now the 40-hex SHA because an explicit `--base` overrides `TEMPER_BASE` |
| BYP-11 | med | ✔ | `[workspace] exclude/members`, path deps; `.rs` never excluded; `04 §2`. **v1.1:** vendor handling made explicit: a `build.rs` or proc-macro anywhere (including `vendor/`) is exec surface; other `vendor/**` changes tier at least `lite` and are never excluded |
| BYP-12 | high | ✔ | Bootstrap: assay trust paths, `assay-self` gate, genesis SHA; `02 §7`, `05` T16 |
| BYP-13 | med | ✔ | Provenance claims never loosen a rule; identity-based; `04 §2.5` |
| BYP-14 | med | ◐ | Accepted except aggregation of PR splitting (deferred, reported in `assay report`); file-list cross-check, closed docs allowlist, `micro` tier; `04 §2`, `05` T17 |

Counts: 62 findings; ✔ 54, ◐ 8 (REL-2, REL-6, QUAL-1, QUAL-2, ARCH-8, ARCH-10, BYP-2, BYP-14), ⏸ items inside ◐ rows (executable witness, second model family, `cargo-mutants`, PR-split aggregation, `merge_group`); ✘ 0 outright.

## 7. Where reviewers disagreed

- **LLM-unavailable default** — SEC-8 wanted fail-closed for agent authors; REL-7 wanted default `warn` because quota exhaustion would otherwise freeze the founder for up to 5 hours. Resolved by making the policy depend on PR *class*: trust-path/exec-surface PRs and repos in `block_on_infra_for` stay `in_progress`; everything else warns visibly, because the LLM stage is advisory until promoted and gates still enforce.
- **Cache** — security wanted no cache restore (poisoning); reliability wanted a cache for latency. v1 resolved it with a restore-only Actions cache; the round-2 review showed the Actions cache can be pre-seeded with the runtime token, so v1.1 uses a digest-pinned OCI warm `target/` built by a trusted main job instead (ADR-21).
- **Duplicate gating** — ARCH-10 wanted to rely on the repo's `rust.yml`; BYP/SEC findings show a PR-editable workflow's result is untrusted. Duplication kept.

## 8. What changed in the top-level promises

- **G1** now states its true assurance level.
- **G2** is staged; it is not a hard CI gate until Weigh-In ships an upstream contract.
- **G3** LLM blockers are advisory until measured.
- **Effort** rose from 19–25 to 28–34 working days in v1 and to **29–37** in v1.1, and the cash-economics section now says plainly that Path A is not justified on cash; Path B is the pre-agreed fallback.
- **Cost** per PR is now **$1.28 AEU** (v1: $0.90, corrected because incremental runs cost 100%), $282–$423/month AEU at 10–15 PRs/day (a lower bound), and Actions minutes ≈ **$48–80/month** (v1: $33–58) are the only cash cost.

A second, fresh-context regression pass on v1 (consistency and arithmetic, plus a second adversarial round) is recorded in `docs/review/02-regression-pass.md`.
