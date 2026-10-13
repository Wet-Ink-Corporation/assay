# 02 · Architecture

Status: **v1.2**.

## 1. Shape

assay is one Rust binary (`assay`) with a subcommand per pipeline stage, a base-defined GitHub Actions workflow pair, a dedicated GitHub App that alone publishes the verdict, and a small Claude Code plugin (local slash commands, reviewer prompt bundles). Each stage can run in a different job with different permissions.

```
 PR event (pull_request_target; workflow file from the DEFAULT branch, config from base_sha;
           NO branches: filter, Z4 decides base-not-protected; `edited` only when changes.base
           is present; every privileged job: if github.run_attempt == 1)
        │
        ▼
 ┌─ job: open ───────────────┐ App token (checks:write on the consumer repo; contents:write on
 │ find-or-create the ONE    │ the ledger repo), no secrets, no PR content. Reuses the existing
 │ App check run for         │ App check run for (head_sha, check name); sets in_progress,
 │ (head_sha, name)          │ external_id = "pending:<run_id>:<seq>"; on `synchronize` only,
 └──────────┬────────────────┘ appends a push record to the ledger; never rewrites started_at
            ▼
 ┌─ job: plan ───────────────┐ token: contents:read, pull-requests:read; no secrets
 │ assay plan                │ reads BASE config at pinned base_sha; PR files via
 │  → run.json (all SHAs     │ `git diff` on fetched objects (never REST file list alone)
 │    pinned), tier, roles   │ NO ledger access (ledger moved to `admit`)
 └──────────┬────────────────┘
   ┌────────┴──────────────┬───────────────────────┬──────────────────────────┐
   ▼                       ▼                       ▼                          ▼
 ┌─ gates-static ─┐ ┌─ gates-lint ────────┐ ┌─ gates-test ────────┐ ┌─ admit (Z4) ────────────────┐
 │ no secrets     │ │ no secrets          │ │ no secrets          │ │ App key + ledger token      │
 │ contents:read  │ │ EXECUTES PR build   │ │ EXECUTES PR tests   │ │ runs ONLY assay; no PR code │
 │ NO PR exec:    │ │ code (clippy,       │ │ (nextest, doctests) │ │ budget + semaphore slot +   │
 │ scanner, TOML  │ │ rustdoc, build.rs,  │ │ separate VM from    │ │ reservation (only for runs  │
 │ manifests,     │ │ proc-macros)        │ │ gates-lint          │ │ that will spend);           │
 │ ratchet math,  │ │ warm target/ from   │ │ warm target/ as     │ │ builds the context pack     │
 │ test inventory │ │ digest-pinned OCI   │ │ gates-lint          │ │ (suppress, dismissals,      │
 │ (syn), weigh-in│ │                     │ │                     │ │ prior findings) from        │
 │ fold           │ │                     │ │                     │ │ App-authored records        │
 └──────┬─────────┘ └─────────┬───────────┘ └──────────┬──────────┘ └──────────────┬──────────────┘
        │                     │                        │                           ▼
        │                     │                        │      ┌─ review (if admitted, tier ≥ micro) ─────┐
        │                     │                        │      │ secret: CLAUDE_CODE_OAUTH_TOKEN          │
        │                     │                        │      │ token: contents:read (no PR read)        │
        │                     │                        │      │ NEVER runs PR code; no Bash tool         │
        │                     │                        │      │ cwd = empty dir; PR tree via --add-dir   │
        │                     │                        │      │ preflight → roles (concurrent, shards    │
        │                     │                        │      │ sequential) → coordinator → verifier;    │
        │                     │                        │      │ egress scan of its own output (T2)       │
        │                     │                        │      └──────────────────┬───────────────────────┘
        │                     │                        │      ┌─ release (Z4; needs admit, review) ──────┐
        │                     │                        │      │ always(), slot held, run_attempt == 1;   │
        │                     │                        │      │ ledger token only; releases the slot by  │
        │                     │                        │      │ CAS (force-with-lease), writes the cost  │
        │                     │                        │      │ record (real cost, or the cap if died)   │
        │                     │                        │      └──────────────────┬───────────────────────┘
        └──────────┬──────────┴────────────────────────┴─────────────────────────┘
                   ▼  (needs: all above; `if: always() && run_attempt == 1`; explicit states)
      ┌─ job: decide (Z4) ─────────────────────────────────────────────────┐
      │ App token (checks:write, pull-requests:write); ledger token; no     │
      │ model secret; runs ONLY assay (digest-pinned); fetches PR objects   │
      │ as data only (hardened, 03 §9)                                      │
      │ re-fetch PR head and base ref; head ≠ run.head_sha or base ref ≠    │
      │ run.base_ref → RunSuperseded, no check or comment write             │
      │ upstream cancelled AND a newer run exists for this head → publish   │
      │ nothing, leave in_progress (R2-5)                                   │
      │ validate (artifact digest == producing job output) → recompute fp,  │
      │ trust paths, tier → evidence check → command records → rubric →     │
      │ take per-head lock → publish check (App) + comments → persist       │
      │ bundle → write pointers to ledger → drop lock                       │
      └─────────────────────────────────────────────────────────────────────┘

 decide-only workflow (base-defined: only events that run the DEFAULT-BRANCH workflow file)
   issue_comment[created] (/assay recheck | waive | override | dismiss | retry | weigh-in-ack)
   | pull_request_target labeled/unlabeled (assay:* labels) | hourly schedule poll (pending-approval)
   → parses a command only from the triggering `created` payload, checks eligibility there and
     appends an App-authored command record to the ledger; reads the evidence pointer from the
     ledger (run_id, run_attempt, sha256), fetches that run's artifact, verifies provenance +
     digest, snapshots approvals and command records, re-runs `decide`, publishes under the
     per-head lock and uploads its own decision bundle. No model call, no PR code.
     `pull_request_review` is NOT a trigger: it runs the workflow file from the PR merge commit.
```

Every job that holds a secret or a write token (`open`, `admit`, `review`, `release`, `decide`, decide-only, `requeue`, `reconcile`) carries `if: github.run_attempt == 1` (for `decide`, `always() && github.run_attempt == 1`) from the first shipped template, and the binary refuses `GITHUB_RUN_ATTEMPT != 1`: a manual re-run would otherwise reuse `admit`'s outputs and spend without admission, launder a flaky gate, or resurrect an old workflow file and digest with today's secrets for 30 days (`05` T22). The recovery path is `/assay recheck` or a new push, which start a fresh run.

Jobs communicate through (a) job `outputs` for integrity data and small control values, written only by steps that PR code cannot reach, and (b) workflow artifacts with **per-job nonce names**. **Normative artifact rule:** every artifact that `decide` or `admit` consumes must have its sha256 published in the *producing job's* `outputs`; Z4 rejects any artifact without a matching output digest. This is the only authentication of the Z2s→Z4 channel, because a hostile Z2b process can use the workflow-run-scoped Actions runtime token to list, overwrite or delete any artifact of the run by name (R2-3). Artifacts from `gates-lint`, `gates-test` and `review` are **untrusted data** even when the digest matches: nothing in them is believed until revalidated or recomputed (`02 §4`).

## 2. Components

| Component | Responsibility | Crate |
|---|---|---|
| Domain | Findings, severities, fingerprints (computed here, only here), rubric, tiers, run state machine, events. No I/O. | `assay-domain` |
| Application | Use cases (`open`, `plan`, `admit`, `gate`, `review`, `decide`, `replay`, `report`, `doctor`), port traits (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`). | `assay-app` |
| Forge adapter | GitHub REST/GraphQL via `gh` (H-01), App-token minting (narrowed per job), diff/objects, checks (published under the per-head ledger lock), comments, timeline events, reactions, ledger repo (contents and refs, including `git push --force-with-lease` for compare-and-set ref updates). | `assay-forge` |
| Runner adapter | Builds and supervises `claude -p` processes; parses `--output-format stream-json --verbose`; cost accounting from `result` messages; auth + session-init preflight. | `assay-runner` |
| Gate adapters | The `temper` and `weigh-in` built-in adapters and the `exec` adapter; read `.assay/plugins.toml`. | `assay-gates` |
| CLI | Composition root, config loading, exit codes. | `assay-cli` |

Six crates. Dependencies point inward: `domain ← app ← {forge, runner, gates} ← cli`. Domain depends on `serde`, `thiserror`, `sha2` only. assay **does not** link `weighin-core` or any temper code (P8).

Plugin directory (`plugin/`): `commands/` (`/assay:check`, `/assay:replay`, `/assay:report`), `prompts/` (reviewer bundles, content-addressed, versioned), `hooks/` (pre-push helper, and the assay mod `hooks/register.tsx`, an in-session view over `assay.status/1` and `assay.event/1`, ADR-25). In CI, prompts are delivered with `--append-system-prompt-file` from the base checkout (not `--plugin-dir`). Following temper's own rule, the assay repo carries no root `CLAUDE.md` that duplicates `AGENTS.md`.

## 3. Trust zones and job classes (P2, P3)

| Class | Handles untrusted code | Handles untrusted text | Has model secret | Has write token | Jobs |
|---|---|---|---|---|---|
| **Z0 open** | no | no | no | `checks:write` on the consumer repo, `contents:write` on the ledger repo (App) | `open` |
| **Z1 plan** | no | metadata only | no | no | `plan` |
| **Z2s static** | no PR-controlled execution (reads, regex, in-process TOML parse, `syn`; pinned *base* tools such as `temper scan` and `cargo metadata --locked --offline` may run on a sanitised manifest-only copy with explicit `CARGO_HOME`/`RUSTUP_TOOLCHAIN` and no `.cargo/config.toml`, `rust-toolchain.toml` or `.gitattributes`; never a build, a build script or a binary from the PR) | yes | no | no | `gates-static` |
| **Z2b build** | **yes** (build.rs, proc-macros, clippy, rustdoc in `gates-lint`; tests in `gates-test`, a different VM) | yes | no | no | `gates-lint`, `gates-test` |
| **Z3 read** | no (reads, never executes; no Bash) | **yes** | **yes** | no | `review` |
| **Z4 write** | no (fetches git objects as data) | validated data only | no | **yes** (App; ledger repo) | `admit`, `release`, `decide`, decide-only, `requeue`, `reconcile`, `canary`, `report` |

Rule: no job is in more than one of {executes PR code, holds the model secret, holds a write token}. Z4 never executes PR code and sees only schema-validated, sanitised data. The Z2b split means a hostile test can lie only about test results; only `build.rs` and proc-macros (owner-approved exec surface, R-07) can lie about clippy and ratchet counts.

**Why `pull_request_target` (ADR-03).** The workflow definition must come from a protected branch so a PR cannot edit away its own gate. On github.com `pull_request_target` runs the **default branch's** workflow file whatever the PR's base (`A-28`, verified), while `.assay/` config is read at `base_sha`; doctor therefore requires every protected base to pin the same assay digest as the default branch (`05 §7`). The PR head is fetched as git objects into a subdirectory (`pr/`) **by immutable SHA**, never `refs/pull/N/merge` or `head.ref`, and never used to select workflow, tool versions, config or prompts. There is **no `branches:` filter**: the workflow runs for every base and `decide` publishes `failure (base-not-protected)` for a base outside `protected_bases`. The check is not required there, so nothing is blocked, and a stale result is fail-safe if the same head is later targeted at a protected base (`neutral` counts as passing and gave a retarget-and-merge window, R3-03). The check is bound to its base: `assay / verdict` is published only for PRs into the default branch, and each other protected base uses `assay / verdict @<base>`, required by that base's own ruleset, so a `success` earned on one base never satisfies another. Every retarget (`edited` with `changes.base`) re-decides, and every `decide` re-fetches `pull.base.ref` and writes no check when it differs from `run.base_ref` (`05` T3/T15).

**The required check is not forgeable by a workflow in the PR.** All workflows in a repository post as the same "GitHub Actions" app, so pinning a ruleset to that app does not distinguish a PR-added workflow from assay. The verdict is therefore published only by the dedicated `assay-verdict` GitHub App (private key in a Z0/Z4-only environment), the ruleset requires that App's `integration_id`, and idempotency lookups match `check_run.app.id` (ADR-11). Rulesets also restrict `.github/workflows/**` and `.assay/**` edits to the owner (`05 §7`). The App's output is not trusted by name either: decide-only reads the bundle pointer only from App-authored ledger state (ADR-23), never by searching artifacts by name (R2-1).

## 4. Where each input comes from and what Z4 recomputes

| Input | Source | Z4 treatment |
|---|---|---|
| assay binary | OCI artifact in GHCR pinned by **digest** in base `.assay/plugins.toml` `[assay]` (private repos; `packages: read` on every job that runs it, `A-20`) | Verified by digest before execution; attestation verification only if the plan supports it (`A-08`) |
| temper, weigh-in tooling | Same: GHCR digest or pinned git **commit SHA** built in a release job, not tags | Digest verified |
| Warm `target/` for Z2b | Digest-pinned OCI artifact built by a trusted main-branch job (`06 §3`); never `actions/cache` | Digest verified; treated as an untrusted input to `cargo clean -p <workspace crates>` |
| Plugin manifest, gate commands, thresholds, `suppress`, `owners`, risk/exclusion globs | Base `.assay/` at `base_sha` | n/a |
| REVIEW.md / AGENTS.md / CLAUDE.md rules for reviewers | Base copies | The PR's versions are shown to `meta` as a diff |
| temper config (`.claude/temper.json`) | PR head, but **compared key-by-key with base** by the temper adapter in Z2s (`03 §2.1`) | Gate-config drift is a failing finding unless the base allowlists it |
| Diff and file contents | PR head objects | The thing under review |
| PR title/body/comments/commit messages | PR | Untrusted; envelope + sanitisation |
| `run.json` (tier, trust paths, plugin list) | Produced by `plan` | **Not trusted by Z4**: Z4 recomputes trust paths, tier, required plugins and base/head SHAs from base config and PR git objects; mismatch ⇒ `failure` `plan-mismatch` |
| Gate and review artifacts | Z2/Z3 jobs | Untrusted: sha256 must equal the producing job's `outputs` digest (rejected otherwise); schema-validated; fingerprints recomputed; evidence rechecked |
| Context pack for Z3 | Built by `admit` (Z4) from App-authored records only (prior bundles by ledger pointer, dismissal command records in the ledger), never from PR comment text | Z3 treats it as data; digest in `admit` outputs |
| Approvals | GitHub reviews at decide time, resolved to numeric user ids; per reviewer, the latest review on `head_sha` whose state is not `COMMENTED` must be `APPROVED` and not dismissed | Snapshotted into the input bundle |
| Waivers, override, dismissals, retry, Weigh-In ack | App-authored **command records** in the ledger (`commands/<repo_id>/<pr>.jsonl`), written by decide-only from the triggering `issue_comment.created` payload only; never comment bodies read later. A record is void if its comment was deleted, edited (`updated_at != created_at`) or its body hash differs (R3-02) | Snapshotted into the input bundle |
| Eligibility lists (`owners`, `approvers`, `maintainers`, `agents`) | Base `.assay/owners` at `base_sha`; decide-only intersects them with the lists at the current tip of the protected base, so a removal takes effect at once and an addition waits for a new run | Recorded in the decision bundle |
| Pusher set and push clock | Commit authors and committers resolved per commit from the git range `base..head` computed in Z4 (`git rev-list`; > 250 commits ⇒ `needs-split`; a commit whose author or committer is not a GitHub user, apart from `web-flow` as committer, counts as authored by every owner) ∪ the `sender_id` of every append-only **push record** for the PR in the ledger (`pushes/<repo_id>/<pr>.jsonl`, `{before, after, sender_id, forced, at}`, written by `open` for `synchronize` only; the `opened` sender is recorded as `opener`), excluding records and commits that are clean merge-from-base (`04 §5`). Push clock = `at` of the latest record whose `after == head_sha` | Compared with the approver (P10) |

## 5. State and keys

- **evidence_key** = `sha256(repo, pr, head_sha, base_sha, config_hash, assay_digest, claude_cli_version, prompt_bundle_hash, tool_pins)`. Identifies expensive evidence (gates + model output). It is computed by `plan`/`decide` (which can read base config), **not** by `open` (which has no `contents` permission). A completed evidence set is reused only through a digest stored in an App-authored **evidence record** in the ledger, `{evidence_key, bundle_sha256, llm: complete|skipped(draft)|deferred|degraded, tier}`, never by searching artifacts by name (R2-1). Gate results are reusable whatever `llm` says; the LLM part only when `llm == complete` and `tier` is at least the tier now needed, so a draft (gates-only) or deferred run never stands in for an LLM review (R3-17). `admit` checks for a reusable record before it reserves or spends (R3-15).
- **run_uid** = `(repo_id, run_id, run_attempt)` identifies one run (`run_attempt` is always 1, §1). Leases, cost records, `reserved` records, deferral records and the check's pending stamp are keyed by `run_uid`; `evidence_key` is used for reuse only, because a requeue, a retry and a same-head run all share it (R3-16).
- **decision_key** = `sha256(evidence_key, approvals_digest, waivers_digest, override_state, dismissals_digest, on_infra_failure_policy, deferral_state)`. Idempotency applies to *publication*: `decide` re-runs (cheaply, no model) whenever the decision_key changes, and is a no-op only when a check run **authored by the `assay-verdict` App** already carries that decision_key, a final (non-deferred) conclusion and no newer `pending:` stamp.
- **Authoritative per-PR state lives in the ledger, not in the check (ADR-23).** GitHub offers no atomic check-run update (`PATCH /check-runs/{id}` takes no expected value), a PATCH overwrites `output` as a whole, and annotations accumulate on every update, so the check run cannot be a state store (R3-09, R3-11). The App-only ledger repo (`06 §1`) holds, per PR: push records, command records, the evidence and decision pointers, the requeue counter and deferral record, retry grants (refs) and the evidence records above. `output.text` is a rendered projection of that state.
- **Per-head publish lock.** Every writer of the verdict check (`open`, `decide`, decide-only, `reconcile`) first creates the ref `refs/heads/lock-<repo_id>-<head_sha>` in the ledger (creation is atomic; 2-minute TTL; released and reaped by compare-and-set, `06 §1`), and holds it across read → decide → write. The stamp is `pending:<run_id>:<seq>` with `seq` taken from the latest push record; a writer publishes only if the stamp it read is unchanged *while it holds the lock*, and a stale run whose stamp is older than the current one yields. "Compare-and-set" on the check itself is not used, because the platform does not provide it (R3-09).
- **Per-PR state on GitHub**: exactly **one** App-authored check run per (head SHA, check name), where the name is `assay / verdict` for the default branch and `assay / verdict @<base>` for other protected bases (§3). `external_id` is two-phase: `open` stamps `pending:<run_id>:<seq>`; `decide` replaces it with `decision_key` under the lock. `open` never rewrites `started_at` on an existing run (the push clock comes from the ledger push record, R3-07). The verdict check carries **no `output.annotations`**; location data travels in inline comments and in the non-required `assay / gate: <name>` checks (R3-11). One summary comment keyed by `<!-- assay-summary -->`, inline comments carrying a hidden marker. Cost records live in the central ledger (`06 §1`), not in per-commit check runs.
- **One PR per head.** `decide` lists `GET /commits/{sha}/pulls`; if more than one open PR on the head targets the same protected base, it publishes `failure ambiguous-head`, because approvals, commands and authorship are per PR while the verdict is per SHA (R3-19).
- **Bundle pointers** (in the ledger, projected into `output.text`), two parts (R3-08):
  - `evidence: {run_id, run_attempt, sha256}`, written by the pipeline's `decide`. Accepted only if the run is of workflow `assay.yml`, event ∈ {`pull_request_target`, `workflow_dispatch`} (for `workflow_dispatch`, `head_branch` = the default branch, R3-04), `head_repository == repo`, `run_attempt == 1`, and the artifact's sha256 equals `sha256` (R2-1, R3-04).
  - `decision: {run_id, sha256}`, written by decide-only for its own decision bundle. Accepted only if the run is of workflow `assay-decide.yml`, event ∈ {`issue_comment`, `pull_request_target`, `schedule`}, `run_attempt == 1`, and the artifact's sha256 equals `sha256`.
- **Concurrency**: the pipeline group is `assay-<repo>-<pr>` with `cancel-in-progress: ${{ github.event.action == 'synchronize' }}`, so label, review-ready and base-change events queue rather than cancel an in-flight review (the `edited` livelock). GitHub keeps one running and one pending entry per group and a newly queued run evicts the pending one, and `types:` cannot filter on `changes.base`, a comment body or a label name, so **group keys are derived from the filter** and events that will be filtered out never enter the real group (R3-10): in the pipeline an `edited` event without `changes.base` goes to `assay-noop-<run_id>`; in decide-only the key is
  ```yaml
  group: ${{ (github.event_name == 'issue_comment' && github.event.issue.pull_request && startsWith(github.event.comment.body, '/assay ') && github.event.sender.type != 'Bot') && format('assay-recheck-{0}', github.event.issue.number) || format('assay-noop-{0}', github.run_id) }}
  ```
  with the same treatment for label events (only `assay:*` labels not applied by the App join the real group). The decide-only group `assay-recheck-<repo>-<pr>` never cancels and sets `queue: max`, so a pending owner command is never evicted. These expressions stay in `concurrency:`, never in `run:` (T19). The pipeline's `decide` has no shared group (the per-head lock arbitrates). **Cancelled upstream**: if `decide` sees an upstream result of `cancelled` and a newer run exists for the same (workflow, head_sha), it publishes nothing and leaves the check `in_progress` for the newer run; otherwise (cancelled by a human) it publishes `failure gate-error`. A superseded head emits `RunSuperseded` and writes no check or comment; its ledger record and slot release still happen (the `release` job, `06 §1`), and superseded SHAs' checks are closed `cancelled` by `reconcile` (not left `in_progress`).
- **Event loops**: App-token events do trigger workflows. decide-only therefore requires `types: [created]`, a `/assay ` command prefix in the closed grammar (`04 §6`), `sender.type != 'Bot'`, and for `labeled` only `assay:*` labels not applied by the App; all writes are write-if-changed. As defence in depth a privileged command whose issue comment has a non-null `performed_via_github_app` is rejected; reviews carry no such field, so this filter cannot cover approvals (R3-01).
- **Persisted input bundle** (`RunInputs`, 90 days): `run.json`, gate documents, review output (full finding bodies and evidence), GitHub state snapshot (approvals, command records, eligibility lists, pusher set), rubric version, decision. Uploaded by `decide`; it is what `assay replay --verify` reads. The pipeline's bundle is the contract `assay.run-inputs/1` (`03 §6a`): one JSON file, `run-inputs.json`, in `decide`'s own `assay-decide-<nonce>` artifact, capped at 96 MiB, whose sha256 is the evidence pointer's and the evidence record's `bundle_sha256`. It is uploaded before the per-head lock is taken; its pointer and record are written under the lock before the check, so the check can show `persist-error`; a refused run (`gate-error`, `plan-mismatch`) persists none, having no authenticated inputs to keep. The GitHub state snapshot joins it with #58. decide-only uploads a small **decision bundle** (its snapshot, its decision and the evidence pointer), and replay follows the chain. Persist failure fails closed for trust-path and exec-surface PRs (`04 §1` S9). A missing artifact (expired or deleted by a writer, F20) is distinct from a digest mismatch (F19).
- **Base is pinned by SHA** at `plan` and carried through; later jobs check out that SHA, never the moving base ref.

## 6. Local mode (R-10, P9)

`assay check --local` runs `plan → gate → review → decide` with a `Forge` that reads the working tree and a `ModelRunner` that uses the interactive logged-in `claude`. Output `.git/assay/<tree-fp>.json` plus terminal report; advisory; CI never reads it. Its two purposes: catch blockers and Weigh-In items before the push, and let a developer see the report without waiting for CI.

As built (#107): `assay check --local [--base <branch>] [--claude <program>]`, run from the repository root.

- **Snapshot and tree fingerprint.** The working tree is snapshotted into a scratch object-only repository whose `objects/info/alternates` borrows the developer's object store. In a scratch index git runs `read-tree HEAD`, `add --all` (with the developer's `.gitignore` and `.git/info/exclude`), `write-tree`, and `commit-tree` with parent `HEAD`, a fixed identity and the epoch as its date. The **tree fingerprint** is that tree's git id. It covers every tracked file with its working-tree content, deletions included, and every untracked file not ignored, so any uncommitted change a push could carry changes it, and an ignored file never does. A clean tree's fingerprint is `HEAD^{tree}`. The developer's index, refs and object store are never written. Hardened git applies no attributes or filters, so an LFS or filtered file shows as its working-tree bytes. The developer's global excludes file is honoured: `core.excludesFile` as their system, user (`HOME`, `XDG_CONFIG_HOME`) and repository config set it, else `$XDG_CONFIG_HOME/git/ignore` or `~/.config/git/ignore`, written with `info/exclude` (1 MiB together) into the scratch repository. A setting that is not an absolute path, an unreadable file or more than 1 MiB refuses the check (exit 2). A sparse working tree (`core.sparseCheckout` set, or skip-worktree entries) is refused; a sparse-checkout file left by `git sparse-checkout disable` is not, since git reads it only while the setting is on, since its snapshot would read every file outside the cone as deleted.
- **Pipeline.** The snapshot commit is planned as a PR's head against `refs/remotes/origin/<branch>` (default `main`), with git's own file list as the REST list. Local mode computes no package sets yet (#65), which `plan` reads as exec surface, so a Rust change is `full` tier. `exec` gates run in the working tree with the 03 §2 environment. Temper and Weigh-In do not run locally: a required one is `gate-error:not-run`, and a required Weigh-In gate is `no-evidence`. The review is one model call on a checkout of the snapshot, never on the working tree, whose ignored files (`.env`) the model must not see. `combined-micro` reviews `micro` and `combined` reviews `lite` and `full`. The CLI runs with no token, so it uses the stored interactive login. Its output is used only when its init shows exactly `Read`, `Grep` and `Glob` and no MCP server. The context pack is the file list (32 KiB) and the diff (120 KiB), each cut at a file boundary with a count of what was left out. Patches are read only when the tier has a review, and their texts are held only up to the 120 KiB budget; a file whose text did not fit is "not shown" while its hunks still place findings. Findings are validated as in Z4 against the snapshot's texts and the gate findings, egress scan included (#87): local mode does not read the CLI's stored login, so it holds no token and scans for unexplained runs only, with every file and symlink of the snapshot (read in batches of at most 8 MiB, a file over 8 MiB or past the 64 MiB index cap left out, which only adds hits) and the base and snapshot commits as what explains a run. Any egress hit makes the review `unavailable` with none of its findings shown, as the CI review job fails on one. No coordinator or verifier runs, so where one is due the decision reports it `unavailable`, and a `full` tier reports its specialist roles `missing`. The decision key is `local:<tree-fp>`.
- **Report.** `<git dir>/assay/<tree-fp>.json` (pretty JSON, written through a temporary file and a rename) has `schema: "assay.local-check/1"` and `advisory: true`, the tree, base and snapshot commits, tier, gates, review and decision. It is not a contract: no schema in `schemas/` describes it, and `ContractValidator` refuses it for every contract, so no CI reader can accept it. No workflow and no other production module names it (`assay-cli`'s `tests/it/check.rs`). The terminal report replaces every control and format character of gate and model text. The command exits 0 on `success` or `neutral`, 1 otherwise, and 2 when the check cannot run.

## 7. Repository layout (assay itself)

```
assay/
  AGENTS.md                    charter (temper template), decisions log
  Cargo.toml  rust-toolchain.toml  deny.toml
  crates/{assay-domain,assay-app,assay-forge,assay-runner,assay-gates,assay-cli}
  plugin/                      Claude Code plugin (commands, prompts, hooks) and the assay mod
                               (hooks/register.tsx, types/, tests/; a view, not a gate, ADR-25)
  workflows/{assay.yml,assay-decide.yml}.tmpl   installed into consumer repos (all actions SHA-pinned)
  rubric/v1.toml
  schemas/                     JSON Schemas for every versioned contract
  corpus/{dev,test,inject}/    eval fixtures (labelled diffs, injection cases)
  docs/architecture/  docs/review/  docs/decisions/  docs/design/
  .claude/temper*, .github/workflows/rust.yml   temper CI (dogfood)
```

For assay's own repo the trust paths additionally include **all of `crates/**`** (forge decides who counts as an owner and what an approval snapshot contains; runner holds the headless profile; app holds the use cases; domain holds rubric, fingerprint and decide logic; gates hold the adapters), `rubric/**`, `plugin/**`, `schemas/**`, `workflows/**`, the release workflow, `.assay/**` and `deny.toml`; and a deterministic **`assay-self` gate** replays `decide` over the golden corpus and fails on any byte difference unless an owner-approved snapshot update is in the PR (`05` T16).

*As built (#76).* The trust paths are base `.assay/globs.toml`'s `trust` list: the paths above, plus `corpus/golden/**`. The gate replays at head, so a rubric or `decide` change that moves a verdict fails it (rule 1) unless the same PR rewrites the affected `decision.json`s; "owner-approved" is then rule 4's, because the rewritten snapshot is itself a trust path whatever caused it (a dependency bump that changes serialisation included). The gate is an `exec` plugin (03 §1, §2), declared in assay's own `.assay/plugins.toml` as

```toml
[[plugin]]
name = "assay-self"
adapter = "exec"
exec = ["cargo", "run", "--locked", "--quiet", "--package", "assay-cli", "--bin", "assay", "--", "self-gate", "corpus/golden", "rubric/v1.toml"]
job = "test"
required = true
```

`assay self-gate <corpus-dir> <rubric-file>` reads both from the head checkout (at most 4,096 entries, 1 MiB per file, 64 MiB in all; symlinks are never followed) and writes one `assay.findings/1` document named `assay-self` to `ASSAY_FINDINGS_OUT`, with `ASSAY_RUN_KEY` as its `run_key`: exit 0 and `pass` when every case decides to exactly its recorded bytes, exit 1 and `fail` with one `blocker` per problem otherwise (`golden-mismatch` at the first differing line, `golden-inputs-invalid`, `golden-case-incomplete`, `golden-unexpected-entry` for anything but `README.md` and case directories holding exactly `inputs.json` and `decision.json`, `golden-corpus-empty`, `golden-rubric-invalid`), and exit 2 with no document when a variable is missing or the tree cannot be read within its caps. `.assay/plugins.toml` itself lands with the first released assay digest, which its `[assay] digest` must name; until then `plan` refuses the repository (`Missing`), as it did before.
