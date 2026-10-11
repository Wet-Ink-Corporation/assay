# 07 · Decisions, assumptions and open questions

Status: **v1.1**. Rollout phases are M0–M6 (`06 §8`).

## 1. Architecture decision records

| ID | Decision | Alternatives considered | Why | Revisit when |
|---|---|---|---|---|
| ADR-01 | assay is its own repo and plugin; temper and Weigh-In contribute gates through two named adapters over documented JSON | Build in temper; monolith | The orchestrator is not a check; temper's Node→Rust port must not leak into assay; no `weighin-core` link (unpublished, pre-1.0) | Never without new evidence |
| ADR-02 | Auth via Max OAuth token in headless `claude -p`, behind a `ModelRunner` port with an API adapter ready | API key from day one; managed Code Review | Owner's goal (existing subscription); port limits vendor risk to one adapter | `A-01` falsified |
| ADR-03 | `pull_request_target` topology: base-defined workflow, PR head fetched as data by SHA; job classes Z0–Z4 | `pull_request` + secrets; `workflow_run` split; GitHub-App-only | A PR cannot edit away its own gate; classes keep secret / code-exec / write token apart | GitHub changes semantics |
| ADR-04 | `decide` is a deterministic pure function; models only emit findings | LLM approve/request-changes | P1; testability; Cloudflare's deterministic rubric | — |
| ADR-05 | One required check `assay / verdict` + non-required per-gate checks | One required check per plugin | Adding a plugin must not need ruleset edits; per-gate required-ness lives in base config | Rulesets become scriptable |
| ADR-06 | assay's own findings schema; temper's *reviewer verdict* is not consumed, only its findings JSON | Reuse `temper_verdict` | Temper verdicts are tree-fingerprint-bound local state, not a CI trust anchor | — |
| ADR-07 | Rust, built under temper rules, six crates | Node/TS (as claude-code-action) | Dogfood (G6); owner's stack. `assay-events` folded into domain/app (the event store is an audit stream, not an aggregate) | — |
| ADR-08 | Mechanical validation (schema, evidence, trigger) precedes and also follows model verification (Z3 non-authoritative, Z4 authoritative) | Verifier alone | Cheap; never ask a model to confirm an unsupported claim | — |
| ADR-09 | Persistence = run-input bundles as 90-day artifacts, referenced by a digest pointer in the App-authored check, + the central ledger repo (ADR-19); telemetry-branch ingest deferred to M5 and conditional | Orphan telemetry branch from day one; external DB; per-commit `assay / cost` check runs (v1, unlistable by time) | Removes a scheduled write job from v1; check runs cannot be listed across SHAs | Artifact retention insufficient |
| ADR-10 | Local mode is advisory; CI never trusts local output | Accept local attestations | Verified: temper verdicts are local `.git/temper/` state | — |
| ADR-11 | The verdict is published by a dedicated `assay-verdict` GitHub App; the ruleset pins its `integration_id` | `GITHUB_TOKEN` check pinned to the Actions app | All workflows share the Actions app, so a PR-added workflow could forge the check (both security and gate-bypass reviewers found this) | If the plan offers required workflows from a central repo (`A-24`), reconsider |
| ADR-12 | Z2 is split three ways: **Z2s static** (no PR-controlled execution; pinned base tools on sanitised copies), **`gates-lint`** (clippy/rustdoc/ratchet counts; runs build.rs and proc-macros) and **`gates-test`** (tests, a different VM); Z4 recomputes everything derivable and accepts an artifact only if its digest equals the producing job's output | One gates job; Z2s/Z2b only | A hostile build script cannot be prevented from lying; keep every decision-relevant check that needs no compilation out of its reach, and let a hostile test lie only about tests. The runtime token lets Z2b rewrite any artifact of the run, so output digests are the authentication | A sandboxed build runner exists |
| ADR-13 | LLM `blocker`s are advisory until the QA-05 promotion rule is met, per repo | LLM blocks from day one | The only published managed review never blocks (neutral check); a same-family verifier is not an independent witness; sample sizes make early precision claims unfalsifiable | QA-05 met |
| ADR-14 | Tooling is delivered by digest/commit SHA via GHCR/OCI, not by release tags or attestations | Release assets + `gh attestation verify` | Cross-repo private downloads need a credential the secretless jobs must not hold; artifact attestations on private repos need Enterprise Cloud (reviewer-cited docs) | `A-20` fails or plan changes |
| ADR-15 | Replay is **decision replay** over a persisted input bundle | Full event-sourced replay | Inputs include live GitHub state; the earlier "100% reproducible" claim was false after artifact expiry | — |
| ADR-16 | Identity separation: agents commit/push under agent identities distinct from owners; every privileged action (approve, waive, override, dismiss, retry, ack) must come from a numeric id in `owners ∪ approvers` that is not the PR author, a GitHub-attributed author/committer, or the recorded push sender (one rule, `04 §5`); a dedicated approver account that never pushes covers owner-authored PRs | Label permission alone; "owner ≠ author" only | Shared credentials void every approval/override control; "not a pusher" is not computable from timeline events, so the push sender and clock are recorded by `open` in the App-authored check | — |
| ADR-17 | Weigh-In CP2 is staged: advisory/experimental until the upstream contract (`D-WI`) exists; blocking set independent of raiser labels; decisions need owner corroboration | Ship CP2 as first drafted | The first draft was an honour system and partly unimplementable | `D-WI` delivered |
| ADR-18 | Path A (this design) is justified by integration + hardening + replay, not by LLM quality or cash; Path B (adopt claude-code-action + pinned temper check) is the pre-agreed fallback | Adopt the action only | The action can already do much of the LLM stage | Kill criteria in `06 §8` |
| ADR-19 | Cost ledger and the cross-repo concurrency limit live in a central private repo (`assay-ledger`): append-only JSONL cost records, and semaphore slots as atomically created refs with a lease | Per-commit `assay / cost` check runs; Actions concurrency groups | Check runs cannot be listed by time or app and vanish with superseded SHAs; concurrency groups are repository-scoped | A second subscription or a hosted store |
| ADR-20 | The decide-only workflow is triggered only by events that execute the default-branch workflow file (`issue_comment` commands, `pull_request_target` `labeled`/`unlabeled`, `schedule`); it reads a bundle only through the pointer in the App-authored check, after verifying run provenance and sha256; approval is a review followed by `/assay recheck` | `pull_request_review` / `pull_request` `labeled` triggers; "latest bundle for the head" | `pull_request_review` runs the PR's own workflow file (App key exposure, silent no-op on the PRs that need approval most); artifact names are a repo-wide namespace a PR-added workflow can write | GitHub adds a base-defined review trigger |
| ADR-21 | The warm Z2b build is a digest-pinned OCI artifact built by a trusted main job, not `actions/cache` | Restore-only Actions cache | The cache can be pre-seeded under the base scope with the runtime token and keys are immutable, so a poisoned entry persists after merge | Actions offers signed/immutable cache scopes |
| ADR-22 | Scoped waivers (`/assay waive <finding-id>`) cover human-decision gate findings only; whole-verdict override is break-glass | Override only | With all allowlists sourced from base, a legitimate raise/exception/test removal can never carry its own allowlist entry, so override would become routine and would also waive unrelated failures | Waivers prove noisy |

## 2. Assumption register

Each assumption names how it becomes a measurement and what changes if wrong.

| ID | Assumption | If wrong | Verified in |
|---|---|---|---|
| A-01 | Using the Max OAuth token in CI for the owner's own private repos is within terms and will remain so; headless usage is not being moved to a separate credit | `ClaudeApi` runner; ≈ $282–$423/month AEU at A-02 volume (lower bound) | Official terms/support article at M0 sign-off, quarterly |
| A-02 | 10 PRs/day (5–15); tier mix 20/25/35/20; 2.5 runs/PR, each at 100% of tier cost (`A-22`) | Cost scales | M1 from `gh pr` history |
| A-03 | 2 concurrent model-running `review` jobs per subscription (central semaphore) fit the window | Reduce to 1; latency rises | M2/M3 |
| A-04 | Typical filtered diff sizes | Tier thresholds shift | M1 history |
| A-05 | Tier thresholds (20 / 150 / 5,000 lines) | Over/under-reviewing | M5 |
| A-06 | Specialists parallelise; per-turn ≈ 10 s; runner start ≈ 80 s | QA-01..04 missed | M2/M3 |
| A-07 | `majors_block_threshold = 3` by distinct root cause | Too lax/strict | M5 |
| A-08 | Artifact attestations are **not** assumed (likely unavailable on private repos without Enterprise Cloud); design uses digest pinning | Add attestations if the plan allows | M1 |
| A-09 | temper's `temper_findings:1` and exit codes (0/1/2) are stable across 1.x→2.0 (the port plan §2.1/§2.3 says so) | Adapter rewrite | Weekly contract test against temper `main` |
| A-10 | The pinned CLI (≥ 2.1.257) supports and honours: `--tools`, `--restricted` (≥ 2.1.248), `--safe-mode`, `--settings` with `permissions.deny` and `permissions.blockReadsOutsideWorkingDirectories` (nested, ≥ 2.1.257), `--add-dir`, `--exclude-dynamic-system-prompt-sections`, `--append-system-prompt-file`, `--json-schema`, `--output-format stream-json` **with `--verbose`** (init + `rate_limit_event` with `status`), `--include-hook-events`, `--permission-mode dontAsk`, `--max-turns`, `--max-budget-usd`, `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB`, `autoContinueAtUsageLimit` (user/managed scope); and `claude auth status` JSON. `--restricted` and `--safe-mode` are assumed combinable (else `--restricted` + `--settings`). The author's machine runs 2.1.296; the three newer names were not found in its bundle, so the doctor verifies them behaviourally | Different isolation (clean `HOME`, container) | M2 `assay doctor` + init assertion + hook and fence canaries |
| A-11 | Weigh-In journal/atom formats (journal `item_id` = atom `weighin_item` = trailer id) are as reviewers reported | Wait for `D-WI` | M4 step 0 reading `crates/weighin` |
| A-12 | Egress restriction in block mode is achievable on GitHub-hosted runners (e.g. harden-runner) | Container/self-hosted runner with network policy before launch | M2 egress-block canary (the review job is first built in M2) |
| A-13 | $40 AEU/day per subscription (≈ 2.1× the 15-PR/day load) is a safe guardrail; plan utilisation is **not** observable with a `setup-token` (scope `user:inference`), so the pause keys on `rejected`/`allowed_warning` rate-limit events only | Recalibrate | M2/M3 |
| A-14 | Specialists and coordinator/verifier use different model tiers; the best split is found by ablation | Single tier | M3 |
| A-15 | Shared system prompt + cwd + tool block + schema gives ≥ 30% input savings **on the specialist calls only** (coordinator and verifier do not share it) | Cost higher | M2 |
| A-16 | `pull_request_target` with explicit per-job permissions, base-pinned workflow, head-as-data and the Z0–Z4 split is safe | Move to a GitHub-App-driven `workflow_run` split | M1 canaries |
| A-17 | A solo owner can run agents under a distinct identity (Claude Code cloud sessions may push through the owner's GitHub authorisation: unverified) | Dedicated approver account (`H-13`) and P10 as convention only | H-08, M1 behavioural check |
| A-18 | Average `full` run ≤ 10% of one 5-hour window | Kill criterion `06 §8` | M3 |
| A-19 | Current SaaS spend is unknown to this spec (third-party figures: ≈ $24–30/seat/month, unverified) | If < ~$100/month, build is not justified on cash (`§4`) | H-11 |
| A-20 | Actions in private repo A can pull a GHCR package from the org's other repo with `GITHUB_TOKEN` (`packages: read`, package access granted to the repo) | Vendor pinned binary into base `.assay/`; or a read-only deploy credential in Z1 only | P1, after the first release; procedure in `docs/operations/release.md` (unverified) |
| A-21 | Role caps and expected costs as in `04 §3` and `06 §2` | Re-size from measured p90 | M2 (lite/micro), M3 (full) |
| A-22 | 2.5 runs/PR; an incremental run costs 100% of the tier cost (specialists re-read the cumulative diff). Lever (4) in `06 §2` would cut it | Cost scales | M2/M3 |
| A-23 | The QA-07 baseline is a pinned build of the official reviewer (`code-review@claude-code-plugins` or built-in `/code-review`, version recorded) | Recompare | Start of M5 |
| A-24 | The GitHub plan (Team vs Enterprise Cloud) determines availability of required workflows and attestations | Design already avoids both | M1 |
| A-25 | `ratchet.raised`, `ratchet.mode` and `exception` fields exist in temper's findings output as read from source; an explicit `--base` overrides `TEMPER_BASE`; `ratchet` degrades silently to totals-only if `git diff` throws | Adapter change | Contract test |
| A-26 | Creating a git ref is atomic and fails if the ref exists, so ref-per-slot is a correct semaphore; the App can hold a separate installation on the ledger repo (per org) | A different lock store | M1 |
| A-27 | A required check with several same-named check runs on one SHA resolves undefinedly, so the design never creates two; the single-run compare-and-set holds | Re-test; keep one-run design regardless | M1 canary |
| A-28 | `edited` payloads carry `changes.base` on a retarget; `issue_comment` and `schedule` run the default-branch workflow file; `pull_request_review` and `pull_request` `labeled` run the merge-commit file (github/docs, read by the round-2 reviewer) | Re-verify; adjust triggers | M1 canary |
| A-29 | A warm `target/` built by the trusted main job is reusable by a PR checkout at a different absolute path with the same pinned toolchain; where it is not (workspace crates), only registry dependencies benefit and Z2b latency in `06 §3` is longer | Longer Z2b; build both at a fixed path (e.g. `/work/ws`) | M1 measurement |

## 3. Human decisions and upstream requests

| ID | Decision / request | Default if unanswered |
|---|---|---|
| H-01 | GitHub client: `gh` CLI behind the `Forge` port, or `octocrab` (a new dependency, which temper's H-16 routes to the owner) | `gh` |
| H-02 / **D-WI** | Ask Weigh-In for (1) the journal hook shipped in the plugin, (2) `weighin ci-check --json --base <ref>` with versioned output and `blocks` as an enum, (3) journal append-only verification. Not on its roadmap or v1 exit criteria today | M4 proceeds `experimental` and non-required |
| H-03 | Adoption order | assay itself, then weigh-in, then temper |
| H-04 | `on_infra_failure` default | `warn`, with `block_on_infra_for = ["trust-path", "exec-surface"]` |
| H-05 | Visibility/licence | Private, internal, `publish = false` |
| H-06 | Owners and approvers | The owner is in `owners`; a dedicated approver account that never pushes is in `approvers` (`H-13`); without it an owner-authored PR can only be merged by a ruleset bypass |
| H-07 | Runner hosting | GitHub-hosted + egress block; container/self-hosted if `A-12` fails |
| H-08 | Agent identity separation (machine user or App for agent commits; agent not in owners). Verify that Claude Code cloud sessions can push under that identity rather than the owner's authorisation | **Required for M1** (the approval rule already needs a non-author, non-pusher approver); until verified, P10 is a convention |
| H-09 | Upstream requests to temper. **D-TEMPER-1** (stop assay parsing files): a `--fail-on-raise` option; counting `exception` hits as failing unless allowlisted; test-inventory and manifest-switch diffs as CLI subcommands; the gate-config diff as a CLI subcommand; a documented findings contract. **D-TEMPER-2** (close the oracle gaps): `gate --base/--findings-out`; comparing blobs rather than hunks (the `.gitattributes` blinding) | assay implements the interim checks itself (`03 §2.1`) |
| H-10 | Delivery by GHCR digest | Yes |
| H-11 | Cash justification: what is the SaaS spend, and is Path B first acceptable? | Start Path B in parallel with M1 only if SaaS spend is high; otherwise M1 first. Answer before M2 |
| H-12 | Create the `assay-verdict` GitHub App and ruleset | Required for M1 |
| H-13 | Create the dedicated approver account (never pushes, not an agent) | Required for M1 if any owner-authored PR will need approval |
| H-14 | Create the private `assay-ledger` repo and grant the App a second installation on it (per org) | Required for M1 |

## 4. Build vs buy and what assay must beat

Reviewer-verified facts on the official options (docs read in the review): `claude-code-action@v1` accepts `claude_code_oauth_token`, `plugins`, `prompt` and arbitrary `claude_args` (so `--json-schema`, `--max-budget-usd`, `--max-turns`); the official `code-review` plugin and the built-in `/code-review` (forked subagent, effort levels, `--comment`, `--max-findings`) exist; `/code-review` does not read REVIEW.md; **managed Code Review** needs Team/Enterprise, averages $15–25 per review and ~20 minutes, publishes an always-neutral check run with machine-readable severity JSON and reads REVIEW.md; `claude ultrareview` bills usage credits ($5–25) after three free runs and is not subscription-absorbed (rejected).

| Option | Gives | Lacks (for these goals) | Cost |
|---|---|---|---|
| Greptile / CodeRabbit | Hosted, indexing, UI | No temper/Weigh-In integration; features unused | Per-seat SaaS (third-party figures ≈ $24–30/seat + usage; **unverified**) |
| Managed Code Review | Official multi-agent review, neutral check + severity JSON, REVIEW.md | Team/Enterprise plan; per-review extra usage; no deterministic gate stack | $15–25/review |
| `claude-code-action` + official plugin (**Path B**) | Subscription-absorbed, official, inline comments, structured output via `--json-schema` | No deterministic decision, tiering, hardened trust zones, Weigh-In/temper contract, replay/eval | ≈ $0 incremental; 2–4 days |
| **assay (Path A)** | Hardened gate stack + tiered LLM + deterministic verdict + Weigh-In + decision replay | Build/maintenance; vendor-terms risk | 29–37 working days + Actions minutes |

**Economics, stated plainly.** Path A's recurring *cash* cost is Actions minutes ≈ $48–80/month (`06 §3`) plus subscription quota; for a single seat at third-party list prices the SaaS is of the same order. **Path A is therefore not justified on cash.** Its justification is control (no data leaves for a third-party index), the temper/Weigh-In integration, a gate that is hardened against the author, and the ability to tune and measure. If the owner's SaaS spend is low, the rational sequence is Path B first (days), then add Path A pieces only where measurement demands them (`H-11`).

## 5. Open questions (non-blocking)

- SARIF output for gate findings (adds `security-events: write` to Z4).
- An **executable witness** for blockers: a failing test or repro command, run in `gates-test`, as a verifier-independent signal that a claimed defect is real (deferred: it needs a safe way to run model-authored code and its cost is unmeasured).
- `cargo-mutants` on changed lines as a Z2b gate for agent-authored PRs (independent signal for agent-written tests; cost/time unmeasured).
- A second model family behind `ModelRunner` for the verifier only, if same-family correlation proves material in the corpus.
- Aggregating PR-splitting by author/day into a tier input (currently reported only).
- Merge queues (`merge_group`) and stacked PRs (both NG6).
- Whether temper should expose its reviewer rubric as a reusable prompt bundle so assay's `design` role cites the same rule IDs without copying them.
