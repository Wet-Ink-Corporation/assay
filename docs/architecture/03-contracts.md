# 03 · Contracts

Status: **v1.2**. All contracts are versioned (`assay.<name>/<n>`), have a JSON Schema in `schemas/`, and are validated on read at every trust boundary (Z2/Z3 → Z4 especially). Unknown fields and unknown versions are rejected on untrusted input. Fields that identify or rank findings (`fp`, `group_id` for gates, `severity` of gate findings) are **never trusted from the producer**; Z4 computes or maps them.

## 1. Base configuration (`.assay/`, read from `base_sha`)

`.assay/plugins.toml`:

```toml
[assay]
digest = "sha256:…"               # assay OCI artifact digest (never a tag)
warm_target = "sha256:…"          # warm target/ OCI artifact built by a trusted main job (06 §3)
claude_cli = "2.1.296"            # exact pinned Claude Code CLI version; doctor requires >= 2.1.257 (A-10)
runner = "claude-oauth"           # claude-oauth | claude-api
protected_bases = ["main"]        # Z4 publishes failure/base-not-protected for any other base; non-default bases get "assay / verdict @<base>"
llm_blockers = "advisory"         # advisory | block (promotion rule QA-05; per repo)
llm_majors = "advisory"           # advisory | block (separate promotion, QA-05; rubric rule 7)
on_infra_failure = "warn"         # warn | block  (LLM stage only, never gates)
block_on_infra_for = ["trust-path", "exec-surface"]   # PR classes (defined below) that always block when the LLM stage is unavailable
ledger_repo = "Wet-Ink-Corporation/assay-ledger"      # central cost ledger and semaphore (ADR-19)

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "git:<40-hex-commit-sha>"   # or "oci:sha256:…"; tags are rejected by the schema
required = true                   # required gates ALWAYS run; there is no applies_when
timeout_s = 1200                  # the adapter's own steps only (scan in gates-static, baseline in gates-lint)

[[plugin.step]]                   # declared base steps; each runs in the named Z2b job under its own timeout
name = "fmt"      ; job = "lint" ; cmd = ["cargo","fmt","--check"]                                        ; timeout_s = 300
[[plugin.step]]
name = "deny"     ; job = "lint" ; cmd = ["cargo","deny","check"]                                         ; timeout_s = 300
[[plugin.step]]
name = "tests"    ; job = "test" ; cmd = ["cargo","nextest","run","--workspace","--no-tests=fail"]        ; timeout_s = 1500
[[plugin.step]]
name = "doctests" ; job = "test" ; cmd = ["cargo","test","--doc"]                                         ; timeout_s = 600

[[plugin]]
name = "weigh-in"
adapter = "builtin:weigh-in"
pin = "git:<40-hex-commit-sha>"
required = false                  # becomes true when D-WI is delivered (04 §9)

[[plugin]]
name = "custom-lint"
adapter = "exec"
exec = ["./scripts/lint.sh", "--json"]
job = "lint"                      # exec gates run in a Z2b job only; their script is PR-controlled by design
required = false
```

**PR classes** (closed vocabulary used by `block_on_infra_for`, tiering and the approval rule): `trust-path` (a changed file matches the trust-path list, `04 §2.2`) and `exec-surface` (a change that adds or alters code which runs at build time: `build.rs`, proc-macro crates, build-dependencies, or a dependency package with a `custom-build` or `proc-macro` target or a changed `source`, `04 §2.2`). A PR may be both. `required_roles` and the run's tier are derived by `plan` and recomputed by Z4 (§4); they are not configuration.

Also in base: `.assay/owners` (TOML: `owners = [{id, login}]`, `approvers = [{id, login}]` and `maintainers = [{id, login}]`, numeric GitHub user ids are authoritative, logins are for display; `owners` and `approvers` list **privileged identities** only, accounts never authenticated in any agent-reachable context: the dedicated approver account that never pushes (`H-13`) and an optional break-glass account; `maintainers` lists the owner's daily account, which may author and push but takes no privileged action; R3-01, `H-06`), `.assay/agents` (agent identities by numeric id; never owners), `.assay/exceptions.toml` (the only way a temper `ask` exception is accepted), `.assay/suppress.toml` (rule ids and path globs to suppress), `.assay/globs.toml` (risk, exclusion, docs-allowlist, trust-path additions). Every one of these paths is itself a trust path. A skipped required gate is a `gate-error`, never a pass.

## 2. Gate contract (adapter → assay)

A gate runs as a process from the manifest, cwd = the head checkout (or, in Z2s, a sanitised manifest-only copy), environment scrubbed to an allowlist (`PATH`, `HOME`, `CI`, `ASSAY_BASE_SHA`, `ASSAY_HEAD_SHA`, `ASSAY_RUN_KEY`, plus adapter-specific variables set by the adapter). It writes one `assay.findings/1` document to a file under the adapter's output directory and exits. For an `exec` gate that file is named by `ASSAY_FINDINGS_OUT` (always set by the adapter, overriding any inherited value), inside a directory created fresh for the run; the document's `source` must be `kind: "gate"` with `name` equal to the plugin's, and a path that is not a regular file is an invalid document:

| Exit | Meaning |
|---|---|
| 0 | Ran; the `conclusion` in the document is authoritative |
| 1 | Ran and failed the gate (document still expected) |
| ≥ 2, signal, timeout, or missing/invalid document | **Gate error** ⇒ `fail` (fail-closed), reason `gate-error:<detail>` |

### 2.1 The temper adapter (verified against temper's source)

What temper actually provides: `baseline --check [--base REF] [--locked] [--findings-out FILE]` (exit 0/1/2) which runs clippy, rustdoc and the scanner against `.claude/temper-baseline.json`, and writes `{temper_findings:1, findings:[{lint, rules[], severity: must|should|consider|unmapped, file, line, message, exception?}], ratchet:{…}}`. `--findings-out` must be under `target/` or `.temper/`. `temper gate` has **no** `--base`, `--locked` or `--findings-out`, mutates the tree without `--check`, and reads its base from `TEMPER_BASE`. An explicit `--base` takes precedence over `TEMPER_BASE`, and `--base origin/<x>` is a moving ref that fails with exit 2 when not fetched. `ratchet()` silently degrades to totals-only when `git diff` throws, in which case `raised` is skipped and `ok` can still be true.

The adapter therefore splits the work by what each step needs, because compilation is the only thing that must run hostile code:

**Z2s (`gates-static`, no PR-controlled execution)**
1. **`raised`**: JSON totals diff of `.claude/temper-baseline.json` at `base_sha` vs head. Any total that increases, or any new entry, is a failing finding. A diff in which every count only goes down is *not* a trust-path change (`04 §2.2`).
2. **`exception`**: the pinned `temper scan --all --json` (a pure source scan) run over the head tree; any finding carrying an `exception` (`ask`) that is not named in base `.assay/exceptions.toml` fails (temper downgrades `#[expect(..)]`-covered hits to `ask`, which its exit code ignores).
3. **Gate-config diff.** Compares `.claude/temper.json` at base and head key by key. The keys the gate reads are `GATE_CONFIG_KEYS = gate, scanner, manifests`; assay additionally compares `protected`, `review`, `skillGate`, `preapprovedDependencies` and `askOnNewDependency` as defence in depth (they steer hooks only). Any difference is a failing finding unless allowlisted in base. (`D-TEMPER-1` asks temper to expose this as a CLI subcommand so assay stops parsing a config file.)
4. **Static checks implemented by assay**, because temper enforces them only in hooks on the author's machine (verified in `hooks.mjs`; `gate`/`baseline` ignore them): (a) *test inventory*: a `syn` parse counts `#[test]`-family attributes and `#[ignore]`/`cfg_attr(.., ignore)` at base and head; any decrease or new ignore fails unless allowlisted (`nextest list` totals from `gates-test` are recorded as an informational cross-check only: they require a build and come from a job that can lie); (b) *manifest switches*: `[lib] test=false`, `[[test]] test=false`, `autotests=false`, `doctest=false`, `harness=false`, `[patch]`, `[workspace] exclude`, `rust-version`, `edition` changes fail unless allowlisted; (c) *lints table diff*: `[lints]`/`[workspace.lints]` parsed as TOML at base and head, any weakening (level lowered, lint removed) fails; (d) *lint inheritance*: every workspace member manifest must carry `[lints] workspace = true`; a new member that omits it fails (this replicates the manifests step of `temper gate` that `baseline --check` does not run). All TOML is parsed in-process. Where a tool is needed (`temper scan`, `cargo metadata --locked --offline` for exec-surface detection) it is the **pinned base tool** running on a sanitised manifest-only copy with explicit `CARGO_HOME`, `RUSTUP_TOOLCHAIN` and `--config` flags and with `.cargo/config.toml`, `rust-toolchain.toml` and `.gitattributes` neutralised, never a binary or configuration from the PR tree.

**Z2b-lint (`gates-lint`)**
5. Runs the **pinned** temper (not `.claude/temper/` from the PR) as `baseline --check --locked --base <base_sha> --findings-out target/temper/findings.json`, with `<base_sha>` the 40-hex SHA (never `origin/<base>`). Used for clippy and rustdoc `must/should/consider` counts. The adapter asserts `ratchet.mode == "diff+totals"`, that no ratchet note contains "only the totals", and that `ratchet.base` equals the merge-base of `base_sha` and `head_sha` as computed independently by assay in Z2s; any mismatch is `gate-error:ratchet-degraded`. `raised` and `exception` entries in this document are unioned into the failures (they can only add), but their absence is never believed.
6. Severity mapping for display only: `must` → `blocker`, `should` → `major`, `consider` → `minor`, `unmapped` → `minor`. `evidence` is synthesised by the adapter from the head file at `file:line`; when the finding is on a deleted line it is read from base. The gate's pass/fail never depends on this mapping.

**Git attribute hygiene (replaces the v1 `.gitattributes` neutralisation, which did not work).** Neutralising attributes with `GIT_ATTR_NOSYSTEM` and an empty `core.attributesFile` leaves the in-tree `.gitattributes` in force, so `* -diff` still blinds temper's changed-line oracle, and GitHub-hosted images register `git-lfs` filters in the system gitconfig, so `* filter=lfs` collapses worktree files to pointers. The adapter and every Z1/Z2s/Z4 git operation instead **forbid**: `git check-attr --source <head_sha> diff text filter working-tree-encoding eol crlf ident` over **every changed path, both sides of each rename**, must report `unspecified` for all; any set, unset or value fails closed with `gate-error:gitattributes`. Scoping by extension does not work: `#[path = "evil.txt"] mod evil;`, `include!` and Cargo's `build = "build.txt"` make any file Rust, so `*.txt -diff` would hide Rust from the oracle. The only exception is a path matching a glob in base `.assay/plugins.toml` `[gates] binary_globs = [...]` (default empty; the glob dialect is `*`, `?`, and `**` / `**/` as whole path segments; read from `base_sha`, never head), meant for binary assets. An allowlisted path is **not silently admitted**: policing returns it in a separate `opaque` list, and downstream gates (the changed-line oracle, tiering, reviewer scoping) treat every opaque changed file as fully changed, because git may show it differently from its content. A NUL-byte "is it binary" check would not be a substitute: Rust string literals can carry NUL. Rust sources, `Cargo.toml`, `Cargo.lock`, `.cargo/*`, `.assay/*`, `rust-toolchain*`, `clippy.toml`, `.clippy.toml`, `rustfmt.toml`, `.rustfmt.toml`, `deny.toml`, `.claude/temper*.json` and `.gitattributes` itself can never be allowlisted, whatever the glob. Environment hygiene is in §9.

7. Declared steps (`[[plugin.step]]`) run in the named Z2b job under their own timeouts: `fmt` and `deny` in `gates-lint`, `tests` and `doctests` in `gates-test` (a different VM, so a hostile test can lie only about test results, and only a hostile `build.rs` or proc-macro can lie about clippy and ratchet counts, which is why those are the owner-approved exec surface). A repository's own `rust.yml` continues to run for developer feedback but is not trusted (a PR can edit it). `--no-tests=fail` closes the template's `--no-tests=pass` hole. This duplicates runner minutes (`06 §3` accounts for it).

### 2.2 The Weigh-In adapter

Specified in `04 §9`; it consumes `weighin ci-check --json` (a versioned contract requested from Weigh-In, `D-WI`). Until that exists the adapter runs in `experimental` mode, non-required, using journals and atoms only.

## 3. Findings (`assay.findings/1`)

```json
{
  "schema": "assay.findings/1",
  "source": {"kind":"gate|reviewer","name":"temper","version":"git:<sha>","role":"bugs","run_key":"…"},
  "conclusion": "pass|fail|neutral",        // gates only; reviewers have no conclusion field
  "findings": [{
    "class": "logic|error-handling|concurrency|resource|security|api-design|layering|tests|docs|compliance|deps",
    "rule": "ERR-04",                        // gate: plugin rule id; reviewer: optional rule-id citation
    "kind": "defect|omission",               // omission: nothing to quote; anchor on enclosing function
    "severity": "blocker|major|minor|nit",   // reviewers: operational definitions in 04 §3
    "introduced_by": "diff|interaction",     // interaction: names changed symbol + unchanged affected caller
    "pre_existing": false,                   // claim; Z4 verifies against the diff
    "file": "crates/x/src/lib.rs", "line": 88, "end_line": 91, "side": "RIGHT|LEFT",
    "evidence": "let x = y.unwrap();",       // verbatim span, ≤ 400 chars; for omission: the function signature
    "expected_behavior": "…",                // required for kind=omission, ≤ 300 chars
    "trigger": "request with empty body reaches line 88 via handler::parse", // required for major+, ≤ 300
    "caller": {"file":"…","line":12,"evidence":"…"},   // required when introduced_by=interaction
    "title": "…",                            // ≤ 100, plain text
    "body": "…",                             // ≤ 1,200, restricted markdown (04 §6)
    "fix": "…",                              // ≤ 600, optional; fences with info string `suggestion` are stripped
    "confidence": "high|medium|low",
    "group_hint": "short root-cause label"   // the coordinator assigns the authoritative group_id
  }],
  "stats": {"files_read": 12}
}
```

- **No producer-supplied `fp`.** Z4 computes `fp = sha256(class ‖ file_at_head (rename-resolved) ‖ enclosing_symbol ‖ norm(first line of evidence) ‖ occurrence_index)[..8 bytes]`, where `norm` is the single shared normaliser (collapse whitespace, unify quote styles, strip trailing comments, treat `…`/`...` as wildcard) also used by the evidence check, and `occurrence_index` disambiguates identical evidence within a file/symbol. `class` is a closed enum, so two runs that describe the same defect in different words still collide on purpose. Dismissals match fuzzily: same file, same enclosing symbol, evidence similarity ≥ 0.8, **and** the same `class` with severity ≤ the dismissed severity; a fuzzy match of higher severity is published with the note "previously dismissed similar (<severity>)" (R3-25). Fingerprints are authoritative only when computed by Z4; Z3 runs the same pure function non-authoritatively, solely to apply `admit`'s suppress and dismissal lists before spending verifier budget (`04 §4` step 6).
- Reviewer findings carry no `conclusion`; a reviewer cannot say "approve".
- Hard length caps are in the schema; over-length input is rejected, not truncated.
- **Free-text egress scan** (T2, T8): in Z3 the assay process, and again in Z4, drop-and-flag any finding whose `title`, `body`, `fix`, `trigger` or `evidence` contains the OAuth token or its common encodings (normalised by stripping non-alphanumerics and comparing windows of ≥ 12 characters, plus base64 and hex forms), or any alphanumeric run ≥ 20 characters that occurs in neither the PR tree nor the base REVIEW.md. Any hit fails the `review` job, raises `injection-suspect` and flags token rotation.
- Display mapping: `blocker` → 🔴, `major` → 🟡, `minor`/`nit` → 🟡 (nit), `pre_existing` → 🟣.

## 4. Run description (`assay.run/1`)

`{run_key=evidence_key, run_uid=(repo_id, run_id, run_attempt), repo, pr, base_sha, head_sha, base_ref, tier, tier_reasons[], classes[trust-path|exec-surface], roles[], required_roles[], shards[], plugins[{name,required,pin,steps[{name,job,timeout_s}]}], trust_paths_changed[], exec_surface_changed[], filtered_stats:{files, lines, excluded[]}, artifact_names:{…per-job nonce…}, draft:bool}`. `budget:{ledger_day_spent_aeu, day_cap_aeu, window_state, allowed, slot, deferral: none|budget-deferred|slot-busy, tier_degraded_from?, reused_evidence?, retry_grant?}` is added by `admit`, not `plan`, and carried in `admit`'s outputs. Informational for Z3; **recomputed by Z4** (including `classes`, `tier`, `required_roles`), except that Z4 accepts `admit`'s full→lite degrade (`tier_degraded_from`) when recomputing required roles, so a degraded run does not read as a missing role (R3-27). `run_uid` keys leases, ledger records and the pending stamp; `evidence_key` is for reuse only (R3-16).

## 5. Decision (`assay.decision/1`)

`{decision_key, evidence_key, conclusion: success|failure|neutral|pending, reasons[{code, detail, fp?}], counts:{blocker,major,minor,nit}, advisory_blockers:[fp], dispositions:[{fp,code}], waived:[{fp,by}], degraded:[{role,reason}], rubric_version, inputs_digest}`.

`pending` is expressed on GitHub as `in_progress` (never `neutral`, which counts as passing for required checks). It is used **only** for quota- or infra-deferred runs (`budget-deferred`, `slot-busy`, `infra-llm-unavailable`) on `trust-path`/`exec-surface` PRs, and for `slot-busy` on any agent-authored PR (`06 §1`), with at most 3 requeues after which the check becomes `failure`. The requeue counter and the deferral record `{repo, pr, head, count, resets_at}` live in the ledger (R3-11, R3-15); the `assay:llm-pending` label is a projection of that record, and such checks are excluded from `reconcile`'s staleness rule while a requeue is scheduled. A PR awaiting owner approval is **not** `pending`: it is `failure (trust-path-unapproved)` with label `assay:needs-owner`, and decide-only flips it when approval arrives. A required role that hit its own cap is `failure (required-role-degraded)`, which waiting cannot heal (`04 §5` rule 8).

Reason codes (closed enum, the cause of a check conclusion): `gate-failed`, `gate-error`, `gate-skipped`, `blocker-verified`, `majors-over-threshold`, `weigh-in-open-blocking`, `weigh-in-no-evidence`, `weigh-in-pending-triage`, `needs-split`, `infra-llm-unavailable`, `llm-degraded`, `required-role-degraded`, `trust-path-unapproved`, `plan-mismatch`, `publish-error`, `persist-error`, `bundle-expired`, `override`, `budget-deferred`, `slot-busy`, `base-not-protected`, `ambiguous-head`, `re-deciding`. (`base-not-protected` is a `failure` on a base where the check is not required, R3-03; `re-deciding` is the fail-safe completed state `open` publishes if a completed check cannot be reopened, `A-30`; `override` carries its scope `llm|all` in `detail`.)

Disposition codes (closed enum, what happened to an individual finding; not all are drops): `schema`, `evidence-missing`, `line-not-in-diff-moved`, `duplicate`, `duplicate-of-gate`, `suppressed-by-rule`, `dismissed-by-owner`, `waived-by-owner`, `verifier-rejected-downgraded`, `cap-exceeded`, `pre-existing`, `injection-suspect`, `no-trigger` (capped at `minor`, not dropped).

(`verifier-rejected-downgraded`: a refuted finding is downgraded to `minor` with the reason attached, not dropped, until the verifier's confusion matrix is measured on the corpus, `04 §4`.) `superseded` is an event (`RunSuperseded`), never a check conclusion or reason: a superseded run writes no check and no comment, but its ledger record and slot release still happen (R3-06).

## 6. Events (`assay.event/1`)

Envelope `{schema, job, idx, ts, run_key, run_uid, stage, kind, data}`. Each job appends to its own JSONL stream (`job`,`idx` order); there is no global `seq` and no inter-job in-memory state. Z4 merges the streams into the persisted bundle. Every state transition emits an event. Kinds:

`RunRequested`, `PlanComputed`, `Admitted{slot, reserved_aeu}`, `PreflightPassed|Failed{auth_method, cli_version, init_clean}`, `GateStarted|Finished{plugin, exit, conclusion, duration_ms}`, `SpecialistStarted|Finished|Failed|BudgetExhausted{role, model, input_tokens, output_tokens, cache_read_tokens, cost_aeu, duration_ms, turns, stop_reason}`, `CoordinatorFinished`, `StageDegraded{stage, reason}`, `RateLimitObserved{status: allowed_warning|rejected, resets_at?}`, `FindingRaised`, `FindingVerified{verdict: confirmed|refuted|unclear}`, `FindingDisposed{code, from?, to?}`, `FindingPublished{url}`, `DecisionMade`, `CheckPublished`, `BundlePersisted{sha256}`, `WaiverApplied{who, fp}`, `OverrideApplied{who, reason}`, `FeedbackReceived{signal: reaction|dismiss|self-dismiss|resolved|anchor-changed}`, `RunSuperseded`, `BudgetDeferred`, `SlotBusy`, `LedgerWritten`, `SlotReleased`, `SlotReaped{run_uid}`, `PushRecorded{before, after, sender_id, forced}`, `CommandRecorded{command, actor_id, comment_id}`, `CommandRejected{command, actor_id, reason}`, `CommandVoided{comment_id, reason: edited|deleted|hash}`, `RetryGranted{role}`, `EvidenceReused{evidence_key}`.

These streams support audit, metrics and the cost ledger (`06 §1`). They are an audit record, not an event-sourced aggregate (the domain `Decider` pattern applies inside `decide`, not across jobs). The verifier's live reject rate (QA-12) is computed from `FindingVerified`.

## 7. Model runner port

```rust
pub trait ModelRunner {
    fn preflight(&self) -> Result<AuthProof, PreflightError>;       // auth method, cli version, init cleanliness
    fn run(&self, call: ModelCall) -> Result<ModelResult, RunnerError>;
}
pub struct ModelCall {
    role: Role,                        // specialist | coordinator | verifier
    system_prompt_file: BundleRef,     // fixed per run, identical across roles
    user_prompt: Prompt,               // context pack first, then role instruction
    schema: &'static JsonSchema,       // one findings schema for all specialists (prompt-cache sharing)
    tools: &'static [Tool],            // Read, Grep, Glob only
    max_turns: u8, budget_aeu: Decimal, timeout: Duration,
    add_dir: PathBuf,                  // read-only PR tree
}
pub enum ModelOutcome { Ok(Structured), BudgetExhausted, TurnsExhausted, Timeout, QuotaExhausted, Malformed }
```

`BudgetExhausted`/`TurnsExhausted` are distinct, non-retried outcomes: the CLI returns no structured output on them, but the `error_max_budget_usd` / `error_max_turns` `result` messages **do** carry `total_cost_usd` and `modelUsage`, so the real cost is ledgered (F2). The role is marked degraded and visible in the check summary. Adapters: `ClaudeHeadless` (OAuth), `ClaudeApi` (API key, same CLI), `Replay` (recorded outputs).

## 8. Headless invocation profile (the Z3 contract)

```
cwd = an empty directory (the same absolute path for every call)
env: CLAUDE_CODE_OAUTH_TOKEN; ANTHROPIC_API_KEY / AUTH_TOKEN / cloud vars UNSET and asserted;
     CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1 (moot with no process-spawning tool; kept as belt and braces)
claude -p < <prompt file: context pack, then role instruction> \   # prompt on stdin, never argv (wi-9fe37e)
  --output-format stream-json --verbose \        # --verbose is required with stream-json under -p; result carries structured_output + total_cost_usd
  --json-schema <schema> \
  --append-system-prompt-file <base-sourced bundle> \
  --exclude-dynamic-system-prompt-sections \     # move per-user/cwd context out of the system prompt (cache reuse)
  --restricted \                                 # >= 2.1.248: no command-running tools, no WebFetch, file tools confined to working dirs, only managed settings + --settings loaded
  --safe-mode \                                  # no CLAUDE.md, plugins, hooks, MCP or skills from anywhere; auth still works
  --settings '{"disableAllHooks":true,"permissions":{"deny":["Read(//proc/**)","Read(//sys/**)","Read(~/**)","Read(//etc/**)"],"blockReadsOutsideWorkingDirectories":true}}' \
  --tools "Read,Grep,Glob" \                     # no Bash at all
  --add-dir <PR tree, read-only>  --permission-mode dontAsk \
  --max-turns <n> --max-budget-usd <aeu_cap>     # = cap exactly (not x1.2); client-side estimate that can overshoot by one response
```

`permissions.blockReadsOutsideWorkingDirectories` is **nested under `permissions`** and requires CLI ≥ 2.1.257 (v1 wrongly put it at the top level, where it is ignored). Under `dontAsk`, reads outside the working directories need no allow rule and are denied by default, so the deny rules and this key are defence in depth; Read-deny coverage of Grep/Glob is best-effort.

Rules:
- **The prompt travels on stdin**, not as an argument (wi-9fe37e): Linux caps one argument at 128 KiB (`MAX_ARG_STRLEN`), which a context pack can exceed, and stdin cannot be mistaken for an option. The runner writes the prompt to an anonymous temporary file before the CLI starts and opens it as the child's stdin (no pipe, so no writer can block). `assay doctor` (#84) verifies that the pinned CLI reads its `-p` prompt from stdin with this flag set.
- **No `Bash`** in Z3. The built-in read-only Bash set (`cat`, `ls`, `grep` …) bypasses `--allowedTools`; `Bash(rg:*)` allows `rg --pre <cmd>` (arbitrary execution) and `git show --output=<path>` writes files (both reproduced by reviewers). `--tools` removes the tool; allowlists do not. `--restricted` removes command-running tools independently. Diff and base-file contents the reviewers need are put in the context pack; Grep/Glob replace `rg`.
- `--bare` is never used (it ignores the OAuth token). `--safe-mode` also disables `--plugin-dir`, so prompts travel as files. `assay doctor` checks that `--restricted` and `--safe-mode` can be combined on the pinned CLI; if not, `--restricted` with `--settings` is used alone.
- The `stream-json` `system/init` message is inspected: any MCP server, plugin, skill or agent ⇒ preflight failure `init-unclean`. **`init` carries no hooks field**, so hooks are excluded structurally (`--restricted` loads only managed settings and `--settings`; `disableAllHooks`) and proven absent by the **hook canary**: a base-sourced canary hook is planted at user, project and in-tree scope in a throwaway checkout, `--include-hook-events` is passed, and the canary fails the doctor if any hook event appears or the marker file is written. A flag that is accepted but ineffective is therefore caught by observation, not by trusting the flag.
- **Fence canary** (monthly and in `assay doctor`): a prompt that asks the model to `Read //proc/self/environ`; the run must show the call in `permission_denials` and the token must not appear in the output.
- `--add-dir` directories count as working directories for `blockReadsOutsideWorkingDirectories` (documented; `assay doctor` still verifies behaviourally; if not, the PR tree is mounted at a path allowed by an explicit `Read(...)` rule instead).
- `assay doctor` verifies each flag against the pinned CLI (`A-10`) and fails on mismatch. The CLI version is pinned in base config and is part of the evidence key.
- `autoContinueAtUsageLimit` (wait-and-resume; user/managed scope only) is off; a usage-limit error is classified `QuotaExhausted`.
- **Rate-limit events.** The stream's `rate_limit_event` carries `rate_limit_info{status: allowed|allowed_warning|rejected, resetsAt?, utilization?}` and fires with `status: allowed` routinely, so assay keys only on `rejected` and `allowed_warning`. A `setup-token` OAuth token carries only the `user:inference` scope and does not expose plan utilisation, so there is no utilisation-based pause (`A-13`).
- **Cost accounting.** `total_cost_usd` exists only in the final `result` message; SIGTERM yields exit 143 with no result record, losing both output and cost. assay therefore does not kill on its own mid-run accounting: the hard stops are `--max-budget-usd = cap`, `--max-turns` and a wall-clock timeout (on timeout the cap is charged as the cost). A `review` job killed from outside (cancelled by a newer push, job `timeout-minutes` ≤ 40) is charged at the run cap by the `release` job, and the per-PR cooldown and 24-hour sum count `admit`'s `reserved` record anyway (`06 §1`, R3-06).
- **Prompt-cache sharing** applies only between calls with the same model, the same system prompt and the same schema/tool block; the specialists share one findings schema, the coordinator and verifier (different schemas, possibly different model tier) do not. `06 §3` models only the shared part.
- The model secret is still present in the claude process's initial environment. `Read(//proc/**)` is denied, no tool can spawn a process, egress is restricted (`05` T2), and the output channel is scanned (§3). This is defence in depth, not a proof (`05` residual).

## 9. Git hygiene for Z1, Z2s and Z4 (fetching PR objects as data)

Every git invocation that touches PR-controlled objects runs with: `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_ATTR_NOSYSTEM=1`; `-c core.hooksPath=/dev/null`; `-c protocol.allow=never -c protocol.https.allow=always`; `-c transfer.fsckObjects=true -c fetch.recurseSubmodules=false -c fetch.fsckObjects=true`; `-c attr.tree=4b825dc642cb6eb9a060e54bf8d69288fbee4904` (the empty tree: a bare repository otherwise reads HEAD's `.gitattributes`, and `--text` does not stop `-diff` from turning numstat counts into `-`; `check-attr --source` still reads the PR's attributes to police them); `--no-ext-diff --no-textconv` on every diff; every git process leads its own process group, its stdio are spooled to anonymous files rather than pipes (so no descendant can hold a reader past the deadline), a run is complete only when the leader has exited and no live process is left in the group, and on timeout or an over-cap output the whole group is killed with the system `kill` and verified gone through `/proc` (a host without a usable `kill` fails closed before any git runs, `06 §5` F22); `-c diff.renameLimit=1000` and a wall-clock cap on `git diff` (`-M` is O(n²); on timeout the PR is `needs-split`, not hung); an **object-only repository with no worktree** in Z4, created in an absent or empty directory (never over an existing repository whose config the flags above would not override). `git diff --numstat` reports `-` for binary files: line counts for tiering use `--text` and treat any remaining `-` as maximum size (forces `full`). The in-tree `.gitattributes` is policed by `git check-attr` (§2.1), not neutralised.
