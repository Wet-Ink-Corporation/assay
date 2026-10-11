# 04 · Pipeline: stages, tiering, reviewers, decision

Status: **v1.1**.

## 1. Stages

| # | Stage | Job | Input | Output | Fails how |
|---|---|---|---|---|---|
| S0 | Open | open | event | the single App check run for (head SHA, `assay / verdict`) found or created, `in_progress`, `external_id = pending:<run_id>`, push sender and clock recorded | Retry ×3; the run aborts visibly |
| S1 | Plan | plan | event, base config at `base_sha` | `run.json` | `failure` (`plan-mismatch` if Z4 disagrees) |
| S1a | Admit | admit | plan, ledger, App-authored records | budget decision, semaphore slot, reservation (only if the run will spend), context pack (suppress list, valid dismissals, prior findings) | Deferred ⇒ `budget-deferred` (rule 8a); admit failure ⇒ LLM unavailable policy |
| S2s | Static gates | gates-static | head objects, pins | `assay.findings/1` per plugin (scanner, `raised`, config diff, inventory, manifests) | Fail-closed |
| S2b | Build gates | gates-lint, gates-test | head tree, pins, warm `target/` | `assay.findings/1` | Fail-closed; retried once on infra-class errors only |
| S3 | Preflight | review | secrets, CLI, diff, context pack | `AuthProof` | LLM stage unavailable ⇒ policy |
| S4 | Specialists | review | context pack | findings | Per role: `BudgetExhausted`/`Timeout` ⇒ role degraded, not retried; `Malformed` ⇒ retry once |
| S5 | Coordinator | review | specialist findings | merged, `group_id`s, ranked | Mechanical merge fallback; marks stage `degraded` |
| S6 | Blind verifier | review | candidates | confirmed/refuted/unclear | Unavailable ⇒ LLM unavailable policy (no silent downgrade) |
| S7 | Decide | decide | persisted inputs + GitHub state | `assay.decision/1` | Fail closed |
| S8 | Publish | decide | decision | App-authored check (CAS on check id + `external_id`), comments | Retry ×3; then `failure` `publish-error` |
| S9 | Persist | decide | bundle | `RunInputs` artifact (90 d); pointer `{bundle_run_id, bundle_sha256}` written into the check | Trust-path and exec-surface PRs: fail closed (`failure persist-error`). Others: warning, and decide-only reports `bundle-expired`/missing and requires a re-run |
| S10 | Ledger | decide | actual cost | central ledger record, semaphore slot released | Warning; `reconcile` reaps expired leases |

S2s/S2b run in parallel with S1a–S6. **Reviewers are not given gate findings**; Z4 removes LLM findings that duplicate a gate finding (same file, line within ±3, same `class`) as `duplicate-of-gate`. This removes the latency dependency and keeps one source of truth. The price is accepted: a finding later removed as a duplicate may already have consumed verifier budget. Dismissals and `suppress` rules, by contrast, are known before the verifier because `admit` hands them to Z3 in the context pack (§4 step 6).

## 2. Plan and tiering (deterministic, S1; recomputed by Z4)

Inputs: the file list from `git diff --name-status base_sha...head_sha` on fetched objects (asserted equal to the REST list when that list is not truncated; the REST list caps at 3,000 files), the parsed manifests at both SHAs, and base `.assay/`. No model.

### 2.1 Filtering

Excluded from model context and line counts, **by base-defined globs only**: lockfiles' text, `*.min.*`, `*.snap`, binary files, and the globs in base `.assay/globs.toml` for generated output. In-file markers such as `@generated` headers are ignored. **`.rs`, `build.rs`, shell/CI/script files and manifests are never excluded**, wherever they live (`vendor/` included). Excluded files are still named to reviewers. Exclusion affects the model's workload only: it never suppresses a gate finding and never hides a trust-path match. Line counts come from `git diff --numstat --text`; a `-` (binary) result is treated as maximum size and forces `full`. A zero-line result that is not in the trivial allowlist (binary-only, snapshot-only, lockfile-text-only with no dependency change) is `micro`.

### 2.2 Trust paths and exec surface (R-07)

Any match forces tier `full`, adds `meta`, and requires the owner approval review (rubric rule 4). Detection runs on the parsed base/head, not just file names, and **matches both sides of a rename** (old path and new path), so moving code out of `**/auth/**` does not leave its risk class. Two reductions keep approval fatigue bounded: a change to `.claude/temper-baseline.json` in which every count only goes down (or is unchanged) is not a trust-path change; and exec surface is detected precisely (below), not by any new package.

**Trust paths:** `.github/**` (workflows, actions, dependabot), `.assay/**`, `.claude/**`, `REVIEW.md`, `AGENTS.md`, `CLAUDE.md` (any directory), `**/CODEOWNERS`, `.gitattributes`, `.githooks/**`, temper's `protected` globs, `clippy.toml`, `.clippy.toml`, `rustfmt.toml`, `.rustfmt.toml`, `rust-toolchain*`, `deny.toml`, `.cargo/**`, `scripts/**`, `xtask/**`, `Makefile`, `justfile`, `package.json`, `Dockerfile*`; and, from TOML-parsed manifests, any change to `[lints]`/`[workspace.lints]`, `[patch]`, `[replace]`, `[workspace] members|exclude`, `rust-version`, `edition`, or the test switches (`test`, `doctest`, `harness`, `autotests`, `autobenches`). If a manifest cannot be parsed, the conservative fallback treats it as changed. Repos add globs in `.assay/globs.toml`. For the assay repo itself: the extras in `02 §7`.

**Exec surface** (compile-time code execution and supply chain), computed by `cargo fetch --locked` followed by `cargo metadata --locked --offline` run by the pinned base cargo on a sanitised manifest-only copy (`03 §2.1`), then diffing the package set base vs head: any `build.rs` (including under `vendor/`); any crate with a `proc-macro` target; `[build-dependencies]`; a **new or bumped package that has a `custom-build` or `proc-macro` target**; any package whose `source` changed (registry/git/path); path-dependency additions. A new or major-bumped *registry* package with no such target is not exec surface: it tiers at least `lite`, adds the `cargo deny` gate, and is routed to the owner by temper H-16's hook as before. Other changes under `vendor/**` tier at least `lite` and are never excluded from review.

### 2.3 Tiers (first match wins, evaluated in this order)

| Tier | Condition | LLM | Expected AEU | Cap |
|---|---|---|---|---|
| 1 `needs-split` | filtered changed lines > 5,000 | none | $0 | $0 |
| 2 `trivial` | every changed file (both sides of a rename) is in the base **docs allowlist** and no trust path or `include_*!` target (below) | none | $0 | $0 |
| 3 `full` | trust path or exec surface changed; or filtered lines > 150; or a base risk glob (`**/auth/**`, `**/crypto/**`, `**/migrations/**`, `**/unsafe*`, `**/*.sql`) | `bugs`, `security`, `design` (+`meta`), coordinator, blind verifier | $1.50 (`meta`: $1.70) | $2.30 ($2.60) |
| 4 `lite` | filtered lines 21–150, or a lockfile dependency change, or a new/bumped registry package, or other `vendor/**` changes | `combined`, blind verifier (≤ 3 candidates) | $0.50 | $0.74 |
| 5 `micro` | everything else: 0–20 filtered lines in any file outside the docs allowlist | `combined-micro` (≤ 8 turns), no verifier; findings capped at `major`, advisory, and never counted by rubric rule 7 | $0.15 | $0.25 |

**Docs allowlist (explicit; replaces v1's `*.md` anywhere and `docs/**` of any type):** root `README.md`, `CHANGELOG.md`, `LICENSE*`, and `docs/**/*.md`, `docs/**/*.txt`, `docs/**/*.{png,jpg,svg}`. **Never trivial**, even inside those globs: `**/SKILL.md`, `**/agents/*.md`, `**/commands/*.md`, `**/hooks/**`, `.kb/**`, `REVIEW.md`, `AGENTS.md`, `CLAUDE.md`, anything under `plugin/**` or `skills/**` (these are executable agent instructions), and any file referenced by `include_str!`, `include_bytes!` or `#[doc = include_str!(..)]` from a `.rs` file at base or head (found by the Z2s `syn` scan; such a README becomes a doctest). Repos extend the allowlist only through `.assay/globs.toml`.

Thresholds are `A-05`, tuned by replay. A repo may add risk globs; it may not lower a tier for trust paths/exec surface. Size is a poor proxy for risk, so code never lands in `trivial`: even a one-character `<`→`<=` change gets the `micro` reviewer.

### 2.4 Budget gate and drafts

`admit` decides (`06 §1`). If projected spend breaches the cap, a **non-trust-path** `full` degrades to `lite` (event `BudgetDeferred`, stated in the summary). A trust-path/exec-surface PR is **never** degraded: its LLM stage is deferred (`budget-deferred`) and the check stays `in_progress` (`pending`) until capacity returns, requeued at most 3 times, after which it fails with `infra-llm-unavailable` so a stuck PR is visible rather than silent. Deferral is distinct from a role that exhausted its own cap (rule 8b). Draft PRs run gates only; the LLM stage runs on `ready_for_review`.

### 2.5 Provenance

Commit trailers (`Claude-Session`, `Co-Authored-By`) and signatures are recorded for telemetry and for the Weigh-In evidence rule's convenience messages. **No rule is looser because provenance claims "human"**: agent sessions can omit trailers and sign with the owner's key. What is strict is identity: agents commit under identities listed in base `.assay/agents` (numeric ids), and rules keyed to "agent-authored" use those ids (`H-08`). Authorship is computed from GitHub-attributed users (GraphQL `commit.authors.user`, the push `sender`), never from `Co-authored-by:` trailers, which anyone can add.

## 3. Reviewer roster (S4–S6)

All specialists share a fixed system-prompt bundle (identical across roles for cache reuse), read-only `Read/Grep/Glob`, the same cwd, and the same context pack placed **before** the role instruction.

**Blindness.** `bugs`, `security` and the `combined` roles receive the diff and tree but **no PR title, body or commit messages** (an agent's own "this is intentional because…" anchors the reviewer). `design` and `meta` receive intent text inside the untrusted envelope. The verifier receives only `{file, line, class, one-sentence claim, trigger}`, never the specialist's rationale, and is a different model tier from the specialists (the split is chosen by ablation, `A-14`).

**Severity, operationally** (in every prompt): `blocker` = causes data loss, security compromise, crash, or an incorrect result in normal production use, with a concrete triggering input or execution path; `major` = incorrect behaviour on a reachable path, or violation of a stated project rule, with a concrete trigger; `minor` = real but low-impact; `nit` = style the formatter does not catch. From `major` up a `trigger` is mandatory; without one the finding is capped at `minor` (`no-trigger`).

| Role | Focus | Tier | Max turns | Cap (AEU) |
|---|---|---|---|---|
| `bugs` | Logic, edge cases, error handling, concurrency, resource bounds; **greps callers of every changed symbol** and traces data flow | full | 25 | $0.60 |
| `security` | Injection, authz, secrets, unsafe deserialisation, new dependencies and `build.rs`; treats PR text as hostile | full | 20 | $0.50 |
| `design` | API shape, layering vs AGENTS.md crate map, test adequacy, docs freshness; cites rust-craft rule IDs from the base cheat-sheet (no shell, no temper agent) | full | 20 | $0.50 |
| `meta` | Only for trust-path/exec-surface changes: what authority the change grants, what check it weakens; primary object is the base→head diff of the trust files | full + trust | 15 | $0.30 |
| `combined` | bugs + security + design in one pass | lite | 20 | $0.50 |
| `combined-micro` | Short, focused pass on a tiny diff | micro | 8 | $0.25 |
| `coordinator` | Group findings by root cause (`group_id`), merge duplicates, apply "what not to flag", rank | full | 10 | $0.30 |
| `verifier` | Blind, one call per candidate blocker/major **or carried-over finding** (together ≤ 5 full, ≤ 3 lite; none in micro); try to refute; `confirmed`, `refuted` or `unclear` plus one line | full, lite | 8 each | $0.08 each (≤ $0.40 / ≤ $0.24) |

Sums: full = 0.60+0.50+0.50+0.30+0.40 = **$2.30**; with `meta` **$2.60**; lite = 0.50+3×0.08 = **$0.74**; micro **$0.25**. Caps are provisional: M2 sets the specialist and lite caps at the measured p90 spend and M3 the full-tier caps (`A-21`). A PR whose pack exceeds 120 KB is sharded by **dependency cluster** (changed files plus their direct importers) into ≤ 3 shards; shards run **sequentially** (so a run never exceeds 4 concurrent model processes), each shard runs the full specialist set, and the run cap scales by the shard count: **$6.90**, or **$7.80** with `meta`.

**Required roles** per tier: full = {`bugs`, `security`, `design`} (+`meta` on trust paths); lite = {`combined`}; micro = {`combined-micro`}. A missing required role, a failed coordinator or an unavailable verifier makes the LLM stage `degraded`/`unavailable` (rubric rule 8). Nothing downgrades silently.

**"What not to flag"** (anti-noise block in every prompt, from Cloudflare/Anthropic practice plus Rust-specific additions): style a formatter handles; anything a linter or gate already reports; pre-existing issues (mark `pre_existing`); speculative performance; naming bikesheds; `unwrap`/`expect` in tests; generated/vendored code; concerns the compiler or type system already guarantees (data races in safe code, use-after-free, exhaustiveness, `Send`/`Sync`); integer-overflow worries with no reachable input; hedged speculation ("could", "might") without a concrete trigger; "add tests/docs" without naming a behaviour. Dismissed-finding classes are reviewed monthly and promoted to `suppress` rules.

**Context pack:** changed-file list with status; filtered unified diff (hunks ±30 lines, ≤ 120 KB); the base REVIEW.md and AGENTS.md; base-side contents of deleted files/hunks (so `side: LEFT` evidence is locatable); prior open findings on re-review (as claims to re-verify). The pack is assembled in two parts: PR-derived material by `review`, and the suppress list, valid dismissals and prior findings by `admit` from App-authored records (never from PR comment text). Specialists may Read/Grep the full head tree.

## 4. Validation and verification (order matters)

Mechanical validation runs as one library function twice: non-authoritatively in Z3 before the verifier (so a model is never asked to confirm an unsupported claim) and authoritatively in Z4.

1. *Schema*; over-length/unknown fields rejected (`schema`).
2. *Evidence location.* The normalised `evidence` span (shared normaliser, `03 §3`) is searched in the **whole** head file, or the base file for `side: LEFT`/deleted code. Zero matches ⇒ drop (`evidence-missing`). One or more ⇒ `line` is **relocated** to the unique nearest match (not dropped for drift). `kind: omission` anchors on the enclosing function's signature, which must exist, and requires `expected_behavior`. Renames are resolved through `git diff -M`.
3. *Placement.* Inline comments require the line to be in a diff hunk. Otherwise the finding moves to the summary (`line-not-in-diff-moved`; it still counts for the rubric). `introduced_by: interaction` findings must cite the changed hunk and a real `caller` span; they anchor on the changed hunk and mention the caller. `pre_existing: true` is honoured only if the cited line is outside the diff **and** `introduced_by != interaction`; otherwise the flag is reset.
4. *Trigger* required from `major` up, else capped at `minor` (`no-trigger`).
5. *Injection screen* (heuristic, last line of defence, not the primary control): URLs outside the allowlist (repo URLs, `rust-lang.org`, `docs.rs`, `owasp.org`), images, HTML, `@mentions`, `suggestion` fences, token-shaped strings, imperative text addressed to the reader/agent ⇒ `injection-suspect`. Two or more in a run raise a visible warning.
6. *Suppression* applies only to LLM findings: base `suppress` rules and owner-valid dismissals (§6) are applied **before** the verifier using the lists in the context pack (Z3 runs the shared fingerprint function non-authoritatively for this; Z4 recomputes), and duplicates of gate findings are removed by Z4 afterwards (§1).
7. *Coordinator* groups by root cause, merges duplicates, applies the cap (≤ 8 inline: blockers first, then majors by confidence).
8. *Blind verifier* per candidate. `refuted` ⇒ **downgraded to `minor` with the reason attached** (not dropped) until the verifier's confusion matrix is measured on the corpus (what share of seeded true positives it kills, what share of known false positives it passes); `unclear` ⇒ one level down; `confirmed` ⇒ unchanged. Every transition emits an event (`FindingVerified`, `FindingDisposed`); QA-12 alerts on a live reject rate outside 10–60% over ≥ 20 runs. Carried-over findings share the verifier cap (§7).

## 5. Decision rubric v1 (S7, pure function)

Inputs: validated gate documents; validated verified findings; the Z4-recomputed plan; the GitHub state snapshot (approvals, waivers, override, dismissals, owners, approvers, agent identities, pusher set, push clock); policy from base config.

```
1  required gate failed | gate-error | gate-skipped (minus findings waived, see below)  → failure
2  base ref not in protected_bases                                    → neutral (base-not-protected)
3  tier == needs-split                                                → failure
4  (trust path | exec surface changed) and not owner_approved(head)   → failure (trust-path-unapproved, label assay:needs-owner)
5  weigh-in gate required and reports open-blocking | pending-triage | no-evidence → failure
6  llm_blockers == block and a verified blocker (not pre_existing)    → failure (blocker-verified)
7  llm_majors == block and count(distinct group_id; major; confirmed by the verifier;
      not pre_existing; from a non-micro role) >= 3                   → failure (majors-over-threshold)
8a LLM stage deferred (budget-deferred | infra-llm-unavailable) and (class in block_on_infra_for or on_infra_failure == block)
                                                                      → pending (in_progress, label assay:llm-pending, requeued ≤ 3 times, then failure infra-llm-unavailable)
8b a required role degraded (own cap/turns/timeout/malformed after retry) and (class in block_on_infra_for or on_infra_failure == block)
                                                                      → failure (required-role-degraded); waiting cannot heal a role cap. Remedy: `/assay retry` by an eligible owner
                                                                        (one re-run with that role's cap ×1.5, recorded) or `/assay override`
8c LLM unavailable or a required role degraded, otherwise            → success + visible llm-degraded warning
9  else                                                               → success
waive    /assay waive <finding-id>: removes ONE human-decision gate finding (raised baseline, ask exception, gate-config drift,
         test-inventory decrease, manifest switch) from rule 1. Never clippy, build, test or deny failures. Recorded in the decision.
override /assay override <reason> → neutral, original reasons retained (break-glass; weekly report)
```

- `owner_approved(head)` (and the eligibility of **every** privileged action: waive, override, dismiss, retry, Weigh-In ack): a numeric GitHub user id in `.assay/owners` ∪ `approvers`, that is **not** (a) the PR author, (b) a GitHub-attributed author or committer of any commit in the PR range, nor (c) the `sender` of a push to this PR recorded by `open`. One rule, used verbatim by R-07, R-08, P10, T9, ADR-16 and §6. Clean merge-from-base commits (a merge of the base tip into the PR branch whose tree equals an automatic merge, checked with `git merge-tree`) are excluded from (b), so the owner pressing "Update branch" does not disqualify their own approval. An approving PR review must also have `commit_id == head_sha` and `submitted_at` later than the **push clock** (the App check run's `started_at` for this head, created by `open`); a comment-based action must be later than the same clock. There is no API that lists the pushers of ordinary pushes, hence the sender recorded in the App-authored check. Co-author trailers are ignored. Because an owner-authored PR has no eligible owner approver, the **approver account** (`H-13`, a second account that never pushes) is the expected approver for those PRs; the alternative is a ruleset bypass outside assay (`06 §4.5`).
- While `llm_blockers == advisory` (default), rule 6 does not fire: verified blockers are published prominently with the 🔴 marker and counted in `assay report` for the promotion decision (`QA-05`). While `llm_majors == advisory` (default), rule 7 does not fire. Both promotions are per-repo base-config changes (a trust-path change, so an H-decision) and each needs its own measured precision.
- A gate finding is never lowered by LLM output or by dismissal; only a scoped waiver removes a human-decision gate finding. Majors are counted by distinct `group_id` (three call sites of one root cause are one). `majors_block_threshold = 3` is `A-07`; a density-aware rule is evaluated on M5 data.
- Override and waiver by whole-verdict break-glass should be rare because waivers cover the legitimate same-PR cases (a human-approved baseline raise, an `#[expect]` exception, a test removal) without waiving unrelated failures such as Weigh-In. The weekly report counts both.
- The rubric is data (`rubric/v1.toml`) plus one pure function; `rubric_version` is recorded.

## 6. Publish, dismissal and feedback (S8)

- **Check run** `assay / verdict`, App-authored (`assay-verdict`), the only check in branch protection, exactly one per (head SHA, name), with `output.summary` (counts, reasons, tier, degraded roles, AEU, the Weigh-In blocking-set digest, how to dismiss/waive/override), `output.annotations` (≤ 50), `output.text` carrying the pointer `{decision_key, bundle_run_id, bundle_sha256, inputs_digest}`, and `external_id = decision_key` (two-phase, `02 §5`). Per-plugin checks `assay / gate: <name>` are additional, non-required.
- **Summary comment** upserted by marker (write-if-changed). **Inline comments** one per published finding with a hidden Z4 fingerprint marker and a footer inviting 👍/👎; a fingerprint is posted at most once per PR; when a carried finding's re-verification says the defect is gone, the thread is resolved with a note.
- **Sanitisation** (Z4, last step): HTML, images, `@mentions` stripped; fenced blocks allowed only with info strings from a small allowlist (never `suggestion`); links only to files/lines of the repo; token-shaped strings and the free-text egress scan (`03 §3`) applied; lengths capped.
- **Dismissal.** `/assay dismiss <fp> <reason>` is honoured only from an eligible owner (§5, P10). It may suppress LLM findings of any severity, **never** gate or Weigh-In findings. A dismissal or thread-resolve by the author/pusher/agent identity is recorded as `FeedbackReceived{self-dismiss|resolved}` and **does not suppress**. Dismissal suppresses by fuzzy fingerprint match for the life of the PR.
- **Approval, waiver, override and dismissal** are re-evaluated by the decide-only workflow. Its triggers are only those that run the default-branch workflow file (`02 §1`): `issue_comment[created]` commands (`/assay recheck | waive | ack | override | retry | dismiss`), `pull_request_target` `labeled`/`unlabeled` on `assay:*` labels, and a `schedule` poll for PRs in `pending-approval`. The expected sequence is: owner approves by review, then posts `/assay recheck`; if they forget, the poll picks the approval up within 20 minutes. No model runs; the bundle is the one the App-authored check points to.
- **Feedback labels** for eval: reactions (response rate is reported alongside precision, since silence is not agreement), owner dismissals-as-wrong, and "a later commit changed the anchor lines".

## 7. Re-review

- A new push is a new run. The **target** the specialists review is always the full cumulative diff; the increment since the last *completed* run (`git range-diff` against the merge-base; the cumulative diff when the previous head is unreachable, e.g. after a force-push, or there is no completed run) is marked "new since last review" to focus attention. This avoids the blind spot where a small push invalidates earlier-reviewed code. Because the specialists re-read the cumulative diff, **an incremental run costs the full tier cost** (`A-02`); only verification of carried findings is incremental.
- Tier is computed on the cumulative diff.
- **Carried-over findings are re-judged**, not string-matched: open findings go to the blind verifier with the question "is this defect still present at head?", sharing the verifier cap with new candidates (≤ 5 full, ≤ 3 lite; carried findings first, by severity). "Anchor line no longer present" means *needs re-verification*, not *resolved*. Carried findings that exceed the cap stay open as of their last verification, are listed in the summary as "not re-verified (cap)", and are excluded from rules 6–7 until re-verified. In `micro` (no verifier) carried findings are re-checked only deterministically (anchor evidence still present at head); if it is, they stay open and advisory; if it is not, they are marked "needs re-verification" and the thread is left open.
- A rebase/force-push resets the carry-over set but keeps dismissal records.
- A dismissed fingerprint (valid dismissal) stays suppressed for the life of the PR.

## 8. Replay and evaluation

- **Decision replay (R-09).** `assay replay --verify <run>` loads the persisted `RunInputs` bundle (including the GitHub state snapshot and full finding bodies) and recomputes `decide`, diffing against the recorded decision. Scope: the decision. Model re-execution is optional and uses a recorded or fresh run explicitly flagged as non-reproducible. Bundles expire after 90 days; replay of an expired bundle is an error, not a pass.
- **Corpus** `corpus/`: ≥ 30 labelled cases at ship (20 dev / 10 test), grown from shadow-period real defects to ≥ 140 (≥ 40 dev, ≥ 100 test) before QA-07 is claimed as a non-inferiority result; until then QA-07 is a descriptive report. Sources: (a) real Wet Ink PRs whose merged code later needed a fix (label = the fixing commit's hunks), (b) mutation-seeded defects (smoke only), (c) true-negative PRs for noise, (d) `corpus/inject/` hostile-text fixtures, (e) rebase/force-push and cross-file "consumer not updated" fixtures. Each case is labelled with the authoring model so same-family (self-review) recall is reported separately from human-authored recall.
- **Dev/test split.** Prompt work uses only `dev`; `test` is not read during prompt iteration and is refreshed from new real defects each quarter.
- **Scorer.** Deterministic: a reviewer finding matches a label if it is in the same file, within ±10 lines of the fixing hunk, and the `class` maps to the label's class. No LLM judge.
- **Method.** k = 3 runs per case. A prompt/rubric change is accepted when no case flips from ≥ 2/3 detected to ≤ 1/3 and the true-negative noise rate does not rise by more than the Wilson-aware margin (reported with n). A mean-difference threshold of "5 points" is not used: one case is 3.3 points at n = 30.
- **Cost.** A pass over 30 defect-skewed cases averages ≈ $1.00 AEU ⇒ **≈ $30**; k = 3 ⇒ **≈ $90 per evaluation**, drawn from a **separate eval ledger** (cap $150 AEU/month), run off-peak, with a cheaper-tier screening pass before the final run. Replay runs never draw on the production daily cap.
- **Shadow mode (M5).** ≥ 4 weeks and ≥ 40 PRs beside the current SaaS. assay posts as its own bot with a separate non-required check (visible, so agents' fixes are not contaminated by only one tool; both tools are visible, accepted). Labelling samples **randomly from all findings of both tools, stratified by tool and agreement, blind to source**, by a fixed protocol; shared false positives therefore get labelled. Report n, intervals and reaction response rate.
- **Promotion** of `llm_blockers` and `llm_majors` follows `QA-05`; there is no rolling-30 test.

## 9. Weigh-In checkpoints (R-11, G2)

**What exists (verified in the weigh-in repo, v0.6.0, 2026-10-10).** The ledger is local (git common dir) and never pushed. Journals under `.kb/_ledger/journal/<session>.jsonl` are written by a repo-local hook (`.claude/hooks/weigh-in-journal.sh`) that only runs in cloud sessions (`CLAUDE_CODE_REMOTE=true`) and is **not part of the plugin** (`plugin/hooks/hooks.json` has no journal hook); local desktop sessions write none. Decisions are atoms in `.kb/_intake/decisions/` and commit trailers `Weigh-In: <id> decided <X>`. Item ids are `sha256(session ‖ normalised question)`, so a marker alone cannot be matched to an item. `blocks:` is free text set by the raising agent; `open --json` is an unversioned dump of the internal `Item`. An omitted or unrecognised `door` parses to `unsure` and `authority` to `authority` (marker parsing), while the *triaged* `door`/`authority` fields are `None` until `/weigh-in` triage; `Status::is_open` covers Raised, Triaged, Grounded, Presented and Deferred, and `Route::Default` items become `Defaulted` after triage and are not open. Nothing is signed.

**Consequence.** CP2 as first drafted (fold journals, match markers in the diff, accept `Weigh-In: none`, trust atoms/trailers) was an honour system and partly unimplementable; and a blocking set built on agent hints would block nearly every PR before triage. G2 is therefore **staged**, and triage state is part of the contract.

**CP1 – local pre-push (ledger present, advisory).** `assay check --local` runs `weighin open --json` filtered to the current branch (`item.branch`) and reports (a) *blocking* items: open and triaged with `door != two-way`, `authority == authority` or a non-null `blocks`, and (b) *pending-triage* items: open with triaged `door`/`authority` still `None`. It blocks the push on (a) and warns on (b). A convenience (P9); `--no-verify` bypasses it.

**CP2 – CI evidence gate (`assay / gate: weigh-in`).** Prerequisite, requested from Weigh-In (`D-WI`, owner H-02): (1) ship the journal hook in the plugin so every session surface commits a journal; (2) `weighin ci-check --json --base <ref>` with a **versioned** output that joins journal `item_id` ∪ atom front-matter ∪ trailer id and reports open items with `status`, **triaged** `door`/`authority` (null when untriaged), `kind` and `blocks` (as an enum `merge|release|none|unknown`; missing = `unknown`, never `none`); (3) append-only verification of journals (below). The adapter rejects any output whose version it does not know (today's `open --json` is unversioned and is therefore used only in `experimental` mode).

assay's rules over that output:
1. **Blocking set** = open, triaged items where `blocks != none` **or** `door != two-way` **or** `authority == authority`, for every `kind` (a one-way authority *question* blocks just as an *assumption* does). **Pending-triage set** = open items whose triaged `door`/`authority` is null: reason `weigh-in-pending-triage`, a warning while the gate is `experimental`, a failure once it is required. The raising agent cannot opt out by labelling, and an untriaged item is never silently treated as non-blocking.
2. **Decisions need corroboration, once per PR rather than once per item.** A decision counts only if the commit adding it is GitHub-verified-signed by a key of an owner, **or** an eligible owner (§5, P10) posts `/assay weigh-in-ack <set_digest>` after the push clock, where `set_digest` is the sha256 of the sorted blocking item ids and their recorded decisions, printed in the check summary. The ack is void when the digest changes. A decision atom, journal event or trailer written by the agent proves nothing by itself.
3. **Evidence rule** (identity-based, per `§2.5`): if the PR author or any pusher/committer is an agent identity (`.assay/agents`) and the PR touches any non-docs file, the PR must contain **at least one** journal, or carry an eligible owner `/assay weigh-in-ack none`. (v1's "a journal for each cited `Claude-Session`" was vacuous for an agent that cites none.) A bare `Weigh-In: none` in the PR body is not evidence.
4. **Deferral** without trigger and resolve-by is open.
5. **Append-only check at event level.** The journal hook's `keep()` deletes a cut-short last line (`sed -i '$d'`) and re-merges after a container resume, and one session id can produce the same journal path in two PRs, so a byte-prefix test is wrong. The check is: every base journal line (by parsed event) is still present at head; a trimmed final partial line and additional lines are allowed; a missing or altered complete line fails.

**Until `D-WI` is delivered** the adapter runs `experimental`: non-required, using documented journal and atom formats (verified in M4 step 0, `A-11`), reporting only. G2 is then enforced by CP1 and by convention (`AGENTS.md`: settle Weigh-In before the session ends), and the summary says so. The marker-in-diff rule is dropped: markers are captured from chat into the ledger, rarely appear in diffs, and cannot be joined to items without the session.
