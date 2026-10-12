88 default(s) taken without you · 0 untriaged item(s) from other sessions
wi-54b5d7 [two-way] Start #46 before the #35 third review merges, although #35 says the review precedes any b… → took: parallel
wi-09dd4c [two-way] Create main from spec/architecture tip 194c6ac as the trunk for story PRs? → took: main
wi-efe727 [two-way] Keep temper's ask-on-config/dependency hook on during the AFK run rather than suppressing… → took: keep-on
wi-a1ed5e [two-way] How should T5 attribute policing handle non-Rust files: police every changed path with a … → took: police-all-with-allowlist
wi-ded964 [two-way] Should allowlisted (binary_globs) changed files that carry diff/filter attributes be admi… → took: opaque-fully-changed
wi-c77f7b [two-way] Should PRs merge once CI is green and rust-reviewer approves, with minor/nit findings fil… → took: merge on green + approval; minors to follow-up issues
wi-3e00e1 [two-way] Is it acceptable that no test pins GroupKiller::at running the /proc preflight (a one-lin… → took: accept untested one-line delegation
wi-5e3297 [two-way] Should the release smoke run stay as bare `assay` (no --version) until the CLI parses arg… → took: keep bare smoke run; switch to --version in the CLI story
wi-469213 [two-way] Should the runner kill claude's whole process group at timeout or cap, given it cannot de… → took: D now, B later
wi-d0697b [two-way] When a model call ends with no readable cost, should it be charged the cap? → took: charge the cap
wi-696446 [two-way] Keep the ModelRunner port shape (borrowed call, associated Error, no preflight/role/tools… → took: keep as built
wi-8aabfb [two-way] Should an override be void on any change to the reasons digest (including a removed reaso… → took: void on any difference
wi-06d367 [two-way] Should decision counts include unwaived gate findings as well as reviewer findings? → took: reviewer plus unwaived gate findings
wi-b9bcde [two-way] Are the waivable temper rule ids (raised, exception, gate-config, test-inventory, manifes… → took: keep names; temper adapter matches or rubric updates
wi-b2ac94 [two-way] For fuzzy dismissal and the fingerprint, should evidence similarity compare the normalise… → took: as built
wi-e1228d [two-way] If assay itself is killed by a signal, claude can outlive it until its own turn/budget ca… → took: accept; setpriv follow-up
wi-5f139f [two-way] Should assay-cli gain a library target now to host ForgeSupervisor before the assay gate … → took: keep the cli library target
wi-bf6ea6 [two-way] Should a draft PR's gates-only decision carry a warning reason (e.g. draft-gates-only) in… → took: status quo
wi-84269b [two-way] Should a draft PR's gates-only decision carry a warning reason instead of a reasonless su… → took: defaulted to the spec (success, no reasons) as wi-bf6ea6
wi-ca74af [two-way] Should a binary numstat '-' force full tier only, rather than counting as maximum size to… → took: force full
wi-fa7bd3 [two-way] Should tool-discovered trust file names (AGENTS.md, clippy.toml, Makefile, package.json, … → took: any directory for tool files
wi-3974c5 [two-way] Should the meta role also be required for exec-surface PRs, not only trust paths? → took: meta for exec surface too
wi-251866 [two-way] Should tool-discovered trust file names match in any directory? → took: defaulted to any directory (wi-fa7bd3)
wi-c8249e [two-way] Should the meta role also be required for exec-surface PRs? → took: defaulted to yes (wi-3974c5)
wi-de9847 [two-way] Should .assay/exceptions.toml use [[exception]] path,lint,reason and [[gate_config]] key,… → took: as built
wi-ed151a [two-way] Should drift in the PR's vendored .claude/temper/ be a major finding rather than failing … → took: major finding
wi-edb21a [two-way] Should an absent base .assay/exceptions.toml be an empty allowlist rather than a gate err… → took: empty allowlist
wi-a29f26 [two-way] Should exec-surface coverage of in-place edits to path build-dependencies and proc-macro … → took: defer to #65
wi-f500a3 [two-way] Should every PR that adds or removes a crate tier full with exec surface (its manifest's … → took: full for new crates
wi-685dea [two-way] Should the temper gate pass a pure removal from the baseline's crates map while #50 tieri… → took: as built
wi-6cb849 [two-way] Should the new blind-spot rule (hidden .rs, submodules, escaping symlinks, cfg_attr allow… → took: non-waivable
wi-deeed4 [two-way] Should a macro metavariable used as an attribute path (#[$m(..)]) outside cfg_attr be fla… → took: Flag as blind-spot (fail closed)
wi-b2f811 [two-way] Should temper blind-spot findings stay non-waivable even though false positives then need… → took: Keep non-waivable
wi-6c128e [two-way] Should the temper adapter fail closed on any non-ASCII character outside literals and com… → took: Fail closed on non-ASCII outside literals/comments
wi-7b372a [two-way] Should a macro_rules transcriber with a fragment followed by '[' ($x [..]) and include!/i… → took: Fail closed on $x [ and non-.rs include targets
wi-6fe092 [two-way] Should FindingDisposed carry the finding fingerprint now (contract change) or should #86 … → took: Keep FindingRef pairing; add fp in a follow-up
wi-c51ac2 [two-way] Should suppress rules be a SuppressRules trait until suppress.toml semantics are specifie… → took: Trait; earlier line wins ties
wi-855abd [two-way] Should a literal include_str!/include_bytes! of a non-.rs file fail the temper adapter's … → took: A: literal data includes pass
wi-b7959c [two-way] What minimum evidence must a model finding cite before step 2 accepts it (04 §4)? → took: At least 12 non-whitespace characters
wi-fd9898 [two-way] Should an interaction finding that cites no hunk line be kept as a summary finding or dro… → took: Keep as summary
wi-641e1a [two-way] Should free-text finding fields be length-capped in validation rather than by adding maxL… → took: Cap in validation; schema later
wi-d4735c [two-way] Should caller.evidence be screened after step 2 replaces it with the tree's line, or shou… → took: Screen model's original caller text
wi-6c9527 [two-way] Should markdown link targets on the 04 §4 allowlist hosts pass, or only repository URLs? → took: Repository URLs only for markdown targets
wi-ce4e56 [two-way] Should a reviewer document whose source.role is outside the roster be rejected as schema? → took: Reject as schema
wi-1d57bb [two-way] For exec-surface detection (#65), should a workspace member with a build script that only… → took: Literal fail-closed readings as built
wi-936f51 [two-way] Should #65 carry #63's forge tree-reading and capture code verbatim (expecting merge conf… → took: Merge #63 first, then #65 with a main merge
wi-cf9060 [two-way] For exec-surface detection (#65), should member version bumps with build scripts, any new… → took: Accepted as built (wi-1d57bb)
wi-9bcfa7 [two-way] Should #65 carry #63's forge tree-reading and capture code verbatim and merge after #63? → took: Accepted; #65 merges after #63 (wi-936f51)
wi-320ab7 [two-way] Should a dependency on a registry other than the default crates.io index count as a plain… → took: Only crates.io is Registry
wi-b5affa [two-way] Should the temper adapter stop modelling rustc's include!/mod/symlink resolution and inst… → took: Refuse rather than model: include!/symlink/shebang blind, anchored mod roots
wi-1d0ae0 [two-way] Should the temper adapter stop modelling rustc's include!/mod/symlink resolution and trea… → took: Defaulted to refuse rather than model (wi-b5affa); plain allow cross-checked ag…
wi-3fd6aa [two-way] Should the pinned cargo refuse a manifest copy whose ancestors hold any .cargo entry, eve… → took: Refuse any ancestor .cargo/Cargo.toml
wi-43d2d7 [two-way] Should the main.rs of a target directory (tests/*/main.rs, src/bin/*/main.rs, examples/*/… → took: count as roots
wi-244533 [two-way] Should the injection screen flag email-shaped text, which GFM renders as a live mailto li… → took: flag email-shaped text
wi-19d034 [two-way] When a symlink under an exec root resolves outside that root, should exec-surface detecti… → took: fail closed to Everything
wi-6c41ca [two-way] Should the temper adapter treat any macro_rules definition that is not at top level, and … → took: refuse both
wi-b5a95f [two-way] Should exec_surface take the declared manifest roots and return the full confined root li… → took: take declared roots
wi-aff4c4 [two-way] Should an out-of-crate [lib] path be lexically normalised so it gives Path(dir) rather th… → took: keep Everything
wi-f352b3 [two-way] Should the injection screen exempt code spans and fences only in plain fields (no pipe, n… → took: plain-field exemption only, cap 256
wi-aef7c3 [two-way] Should findings past 256 in one document be dropped individually as cap-exceeded rather t… → took: drop individually as cap-exceeded
wi-ffd5c4 [two-way] Should the user@sha check require a word boundary before the user, or flag any [A-Za-z0-9… → took: flag any run
wi-8b655a [two-way] Should the injection screen exempt fenced code only for an allowlist of inert languages (… → took: inert-language allowlist
wi-fccfd9 [two-way] Should exceptions.toml allowlist entries for test-inventory and manifest-switch be bound … → took: bind to exact blobs
wi-44b2d6 [two-way] Should Z2s use the fail-closed extensions: per-file inventory, inheritance for every pack… → took: fail-closed extensions
wi-9fd143 [two-way] Should a failed coordinator or unavailable verifier on a blocking-class PR be failure req… → took: failure required-role-degraded
wi-98c82b [two-way] Should a full-tier coordinator marked not-needed be accepted only when there are no revie… → took: not-needed only without findings
wi-9a9002 [two-way] Should DecisionInputs carry completed_roles, coordinator and verifier as top-level fields… → took: top-level fields
wi-6b698e [two-way] Should every symlink or submodule anywhere in the tree make an include target unplaced, o… → took: every symlink and submodule
wi-c19481 [two-way] May removing_the_workspace_manifest_weakens_its_lints also expect the manifest-switch fin… → took: expect both findings
wi-763d92 [two-way] Should dropping any include! or nested mod fail test-inventory as an unplaced module refe… → took: refuse on all
wi-f77e6e [two-way] Are the #64 work-budget sizes right (depth 16, 1 MiB condition text, 18x token scans, 100… → took: caps as built
wi-9c19cc [two-way] Should #55 defer the `assay plan` binary subcommand to #53/#60 (inputs not built yet)? → took: defer subcommand; follow-up issue
wi-c8f67b [two-way] Until #64's include scan is wired, should plan treat every changed path as an include tar… → took: nothing trivial until #64 wired
wi-b9d0d8 [two-way] Should a diff past its cap fail plan rather than emit a needs-split run? → took: fail plan
wi-0334fc [two-way] Accept the run-1 schema edits (needs-split tier; plugin carries pin xor job) and seven ar… → took: accept schema edits
wi-30934c [two-way] Truncated REST list checked as a subset; drafts keep required_roles with roles=[]; packag… → took: accept as built
wi-49768d [two-way] Should an uncertain file refuse dropping any mod (inventory flag) rather than storing its… → took: flag refuses dropped mods
wi-cb2107 [two-way] Are 256 nested brackets and a 32 MiB closure read budget the right caps? → took: caps as built
wi-486e10 [two-way] Are the #64 round-4 caps right (expect list 256 tokens, 10k shared walk visits, 4 KiB / 2… → took: caps as built
wi-e67d58 [two-way] Keep two unplaced reference kinds (Unresolvable, NestedMacro) instead of an Include kind … → took: two kinds
wi-ce5023 [two-way] Are the #64 round-5 budgets right (64 MiB walk work, 32 MiB queued closure paths, 10,000 … → took: budgets as built
wi-93e2fd [two-way] Should corpus/golden/** be a trust path of the assay repo, beyond the 02 §7 list? → took: keep corpus/golden/** as a trust path
wi-10306c [two-way] Should .assay/plugins.toml wait for the first released assay digest instead of a placehol… → took: wait for the first released digest
wi-dd46b2 [two-way] Is a hand-parsed `assay self-gate` subcommand acceptable until the CLI adopts clap? → took: hand parser now, clap with the second subcommand
wi-459016 [two-way] Should a digest mismatch on one gate's document refuse the whole run rather than only tha… → took: refuse the whole run
wi-8e7f04 [two-way] Is the #56 job-outputs contract (JobResult plus sha256 per artifact; gates as plugin to C… → took: keep the #56 shape; #60 conforms or amends it
wi-04d610 [two-way] Should decide refuse review documents from a review job that failed, not only one that wa… → took: refuse only a skipped review job's documents
wi-2506c6 [two-way] Is 256 bytes the right cap on a function name before a cited file is left unindexed? → took: 256 bytes
