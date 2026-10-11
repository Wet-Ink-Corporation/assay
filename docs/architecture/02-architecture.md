# 02 · Architecture

Status: **v1.1**.

## 1. Shape

assay is one Rust binary (`assay`) with a subcommand per pipeline stage, a base-defined GitHub Actions workflow pair, a dedicated GitHub App that alone publishes the verdict, and a small Claude Code plugin (local slash commands, reviewer prompt bundles). Each stage can run in a different job with different permissions.

```
 PR event (pull_request_target; workflow comes from the BASE commit; NO branches: filter,
           Z4 decides base-not-protected; `edited` only when changes.base is present)
        │
        ▼
 ┌─ job: open ───────────────┐ App token (checks:write on the consumer repo only), no secrets,
 │ find-or-create the ONE    │ no PR content. Reuses the existing App check run for (head_sha,
 │ App check run for         │ "assay / verdict"); sets in_progress, external_id =
 │ (head_sha, name)          │ "pending:<run_id>"; records push sender + push clock (started_at)
 └──────────┬────────────────┘ in check output.text
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
        └──────────┬──────────┴────────────────────────┴─────────────────────────┘
                   ▼  (needs: all above; `if: always()`; explicit states for cancelled/skipped/failed)
      ┌─ job: decide (Z4) ─────────────────────────────────────────────────┐
      │ App token (checks:write, pull-requests:write); ledger token; no     │
      │ model secret; runs ONLY assay (digest-pinned); fetches PR objects   │
      │ as data only (hardened, 03 §9)                                      │
      │ re-fetch PR head; head ≠ run.head_sha → RunSuperseded, write nothing│
      │ upstream cancelled AND a newer run exists for this head → publish   │
      │ nothing, leave in_progress (R2-5)                                   │
      │ validate (artifact digest == producing job output) → recompute fp,  │
      │ trust paths, tier → evidence check → rubric → CAS-publish check     │
      │ (App) + comments → persist bundle → ledger write, release slot      │
      └─────────────────────────────────────────────────────────────────────┘

 decide-only workflow (base-defined: only events that run the DEFAULT-BRANCH workflow file)
   issue_comment[created] (/assay recheck | waive | ack | override) | pull_request_target
   labeled/unlabeled (assay:* labels) | schedule poll (PRs in pending-approval)
   → reads the bundle pointer from the App-authored check (bundle_run_id, bundle_sha256),
     fetches that run's artifact, verifies run provenance + digest, snapshots approvals / waivers /
     override / dismissals, re-runs `decide`, CAS-updates the check. No model call, no PR code.
     `pull_request_review` is NOT a trigger: it runs the workflow file from the PR merge commit.
```

Jobs communicate through (a) job `outputs` for integrity data and small control values, written only by steps that PR code cannot reach, and (b) workflow artifacts with **per-job nonce names**. **Normative artifact rule:** every artifact that `decide` or `admit` consumes must have its sha256 published in the *producing job's* `outputs`; Z4 rejects any artifact without a matching output digest. This is the only authentication of the Z2s→Z4 channel, because a hostile Z2b process can use the workflow-run-scoped Actions runtime token to list, overwrite or delete any artifact of the run by name (R2-3). Artifacts from `gates-lint`, `gates-test` and `review` are **untrusted data** even when the digest matches: nothing in them is believed until revalidated or recomputed (`02 §4`).

## 2. Components

| Component | Responsibility | Crate |
|---|---|---|
| Domain | Findings, severities, fingerprints (computed here, only here), rubric, tiers, run state machine, events. No I/O. | `assay-domain` |
| Application | Use cases (`open`, `plan`, `admit`, `gate`, `review`, `decide`, `replay`, `report`, `doctor`), port traits (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`). | `assay-app` |
| Forge adapter | GitHub REST/GraphQL via `gh` (H-01), App-token minting (narrowed per job), diff/objects, checks (CAS by check id + `external_id`), comments, timeline events, reactions, ledger repo. | `assay-forge` |
| Runner adapter | Builds and supervises `claude -p` processes; parses `--output-format stream-json --verbose`; cost accounting from `result` messages; auth + session-init preflight. | `assay-runner` |
| Gate adapters | The `temper` and `weigh-in` built-in adapters and the `exec` adapter; read `.assay/plugins.toml`. | `assay-gates` |
| CLI | Composition root, config loading, exit codes. | `assay-cli` |

Six crates. Dependencies point inward: `domain ← app ← {forge, runner, gates} ← cli`. Domain depends on `serde`, `thiserror`, `sha2` only. assay **does not** link `weighin-core` or any temper code (P8).

Plugin directory (`plugin/`): `commands/` (`/assay:check`, `/assay:replay`, `/assay:report`), `prompts/` (reviewer bundles, content-addressed, versioned), `hooks/` (pre-push helper). In CI, prompts are delivered with `--append-system-prompt-file` from the base checkout (not `--plugin-dir`). Following temper's own rule, the assay repo carries no root `CLAUDE.md` that duplicates `AGENTS.md`.

## 3. Trust zones and job classes (P2, P3)

| Class | Handles untrusted code | Handles untrusted text | Has model secret | Has write token | Jobs |
|---|---|---|---|---|---|
| **Z0 open** | no | no | no | `checks:write` on the consumer repo (App) | `open` |
| **Z1 plan** | no | metadata only | no | no | `plan` |
| **Z2s static** | no PR-controlled execution (reads, regex, in-process TOML parse, `syn`; pinned *base* tools such as `temper scan` and `cargo metadata --locked --offline` may run on a sanitised manifest-only copy with explicit `CARGO_HOME`/`RUSTUP_TOOLCHAIN` and no `.cargo/config.toml`, `rust-toolchain.toml` or `.gitattributes`; never a build, a build script or a binary from the PR) | yes | no | no | `gates-static` |
| **Z2b build** | **yes** (build.rs, proc-macros, clippy, rustdoc in `gates-lint`; tests in `gates-test`, a different VM) | yes | no | no | `gates-lint`, `gates-test` |
| **Z3 read** | no (reads, never executes; no Bash) | **yes** | **yes** | no | `review` |
| **Z4 write** | no (fetches git objects as data) | validated data only | no | **yes** (App; ledger repo) | `admit`, `decide`, decide-only, `requeue`, `reconcile`, `canary`, `report` |

Rule: no job is in more than one of {executes PR code, holds the model secret, holds a write token}. Z4 never executes PR code and sees only schema-validated, sanitised data. The Z2b split means a hostile test can lie only about test results; only `build.rs` and proc-macros (owner-approved exec surface, R-07) can lie about clippy and ratchet counts.

**Why `pull_request_target` (ADR-03).** The workflow definition must come from the base so a PR cannot edit away its own gate. The PR head is fetched as git objects into a subdirectory (`pr/`) **by immutable SHA**, never `refs/pull/N/merge` or `head.ref`, and never used to select workflow, tool versions, config or prompts. There is **no `branches:` filter**: the workflow runs for every base and `decide` publishes `neutral (base-not-protected)` for a base outside `protected_bases` (a non-required check on a non-protected base). A filter would let a retarget leave an earlier `success` on the same head SHA; instead every retarget (`edited` with `changes.base`) re-decides and overwrites the single App check run (`05` T3/T15).

**The required check is not forgeable by a workflow in the PR.** All workflows in a repository post as the same "GitHub Actions" app, so pinning a ruleset to that app does not distinguish a PR-added workflow from assay. The verdict is therefore published only by the dedicated `assay-verdict` GitHub App (private key in a Z0/Z4-only environment), the ruleset requires that App's `integration_id`, and idempotency lookups match `check_run.app.id` (ADR-11). Rulesets also restrict `.github/workflows/**` and `.assay/**` edits to the owner (`05 §7`). The App's output is not trusted by name either: decide-only reads the bundle pointer only from the App-authored check run, never by searching artifacts by name (R2-1).

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
| Context pack for Z3 | Built by `admit` (Z4) from App-authored records only (prior bundles by run id, dismissals recorded by decide), never from PR comment text | Z3 treats it as data; digest in `admit` outputs |
| Approvals, waivers, override, dismissals | GitHub reviews/comments/labels at decide time, resolved to numeric user ids | Snapshotted into the input bundle |
| Pusher set and push clock | GitHub-attributed commit authors and committers (GraphQL, numeric ids; clean merge-from-base commits excluded) ∪ the `synchronize` sender and `started_at` recorded by `open` in the App-authored check | Compared with the approver (P10) |

## 5. State and keys

- **evidence_key** = `sha256(repo, pr, head_sha, base_sha, config_hash, assay_digest, claude_cli_version, prompt_bundle_hash, tool_pins)`. Identifies expensive evidence (gates + model output). It is computed by `plan`/`decide` (which can read base config), **not** by `open` (which has no `contents` permission). A completed evidence set is reused only through a digest stored in an App-authored record (`bundle_sha256`), never by searching artifacts by name (R2-1).
- **decision_key** = `sha256(evidence_key, approvals_digest, waivers_digest, override_state, dismissals_digest, on_infra_failure_policy, deferral_state)`. Idempotency applies to *publication*: `decide` re-runs (cheaply, no model) whenever the decision_key changes, and is a no-op only when a check run **authored by the `assay-verdict` App** already carries that decision_key, a final (non-deferred) conclusion and no newer `pending:` stamp.
- **Per-PR state on GitHub**: exactly **one** App-authored check run per (head SHA, `assay / verdict`). `external_id` is two-phase: `open` stamps `pending:<run_id>` (head and base SHAs in `output.text`); `decide` replaces it with `decision_key` using compare-and-set on `check_run.id` and the previously read `external_id` (a stale run whose `pending:<run_id>` is older than the current stamp yields). One summary comment keyed by `<!-- assay-summary -->`, inline comments carrying a hidden marker. Cost records live in the central ledger (`06 §1`), not in per-commit check runs.
- **Check output pointer**: on every final publish `decide` writes `{decision_key, bundle_run_id, bundle_sha256, inputs_digest}` into `output.text`. decide-only trusts a bundle only if the run with `bundle_run_id` is of the assay workflow file, event `pull_request_target`, `head_repository == repo`, and the artifact's sha256 equals `bundle_sha256` (R2-1).
- **Concurrency**: the pipeline group is `assay-<repo>-<pr>` with `cancel-in-progress: ${{ github.event.action == 'synchronize' }}`, so label, review-ready and base-change events queue rather than cancel an in-flight review (the `edited` livelock). The pipeline's `decide` has no shared group (CAS arbitrates). The decide-only group `assay-recheck-<repo>-<pr>` never cancels; GitHub's replacement of a pending job by a newer pending job is harmless because decide-only reads current state at execution time, so the surviving job subsumes the replaced one. **Cancelled upstream**: if `decide` sees an upstream result of `cancelled` and a newer run exists for the same (workflow, head_sha), it publishes nothing and leaves the check `in_progress` for the newer run; otherwise (cancelled by a human) it publishes `failure gate-error`. A superseded head emits `RunSuperseded` and writes nothing; superseded SHAs' checks are closed `cancelled` by `reconcile` (not left `in_progress`).
- **Event loops**: App-token events do trigger workflows. decide-only therefore requires `types: [created]`, a `/assay ` command prefix, `sender.type != 'Bot'`, and for `labeled` only `assay:*` labels not applied by the App; all writes are write-if-changed.
- **Persisted input bundle** (`RunInputs`, 90 days): `run.json`, gate documents, review output (full finding bodies and evidence), GitHub state snapshot (approvals, waivers, override, dismissals, owners, pusher set), rubric version, decision. Uploaded by `decide`; it is what `assay replay --verify` reads. Persist failure fails closed for trust-path and exec-surface PRs (`04 §1` S9).
- **Base is pinned by SHA** at `plan` and carried through; later jobs check out that SHA, never the moving base ref.

## 6. Local mode (R-10, P9)

`assay check --local` runs `plan → gate → review → decide` with a `Forge` that reads the working tree and a `ModelRunner` that uses the interactive logged-in `claude`. Output `.git/assay/<tree-fp>.json` plus terminal report; advisory; CI never reads it. Its two purposes: catch blockers and Weigh-In items before the push, and let a developer see the report without waiting for CI.

## 7. Repository layout (assay itself)

```
assay/
  AGENTS.md                    charter (temper template), decisions log
  Cargo.toml  rust-toolchain.toml  deny.toml
  crates/{assay-domain,assay-app,assay-forge,assay-runner,assay-gates,assay-cli}
  plugin/                      Claude Code plugin (commands, prompts, hooks)
  workflows/{assay.yml,assay-decide.yml}.tmpl   installed into consumer repos (all actions SHA-pinned)
  rubric/v1.toml
  schemas/                     JSON Schemas for every versioned contract
  corpus/{dev,test,inject}/    eval fixtures (labelled diffs, injection cases)
  docs/architecture/  docs/review/  docs/decisions/
  .claude/temper*, .github/workflows/rust.yml   temper CI (dogfood)
```

For assay's own repo the trust paths additionally include **all of `crates/**`** (forge decides who counts as an owner and what an approval snapshot contains; runner holds the headless profile; app holds the use cases; domain holds rubric, fingerprint and decide logic; gates hold the adapters), `rubric/**`, `plugin/**`, `schemas/**`, `workflows/**`, the release workflow, `.assay/**` and `deny.toml`; and a deterministic **`assay-self` gate** replays `decide` over the golden corpus and fails on any byte difference unless an owner-approved snapshot update is in the PR (`05` T16).
