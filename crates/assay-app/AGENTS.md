# Crate charter: `assay-app`

## Purpose
The use cases (`open`, `plan`, `admit`, `gate`, `review`, `decide`, `replay`, `report`, `doctor`) and the port traits they need (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`). It is its own crate because the ports must be owned by their consumer so adapters depend inward (ARC-03, `docs/architecture/02-architecture.md` §2).

## Layer
application. Orchestrates domain decisions and effects through ports. No concrete adapter, no HTTP or process types, no runtime choice, no `anyhow` in `pub` signatures.

## Allowed dependencies
- Workspace crates: `assay-domain` only, added by the story that first uses it.
- Third-party: crates on the pre-approved list in `.claude/temper.json`, each justified in the root decisions log when added.
- Never: `assay-forge`, `assay-runner`, `assay-gates`, `assay-cli`, `weighin-core` or temper code (P8). `crates/assay-cli/tests/it/architecture.rs` fails on a violation.

## Public surface
Workspace-internal, not published (`publish = false`). Re-exported from `lib.rs`:
- `Contract`, `ContractValidator` (`new`, `read`), `SchemaViolation` (`instance_path`, `keyword`, `message`), `MAX_DOCUMENT_BYTES`: validate-on-read for the versioned contracts, against the schemas in `schemas/` embedded with `include_str!` (`src/contract.rs`). `read` refuses a document over `MAX_DOCUMENT_BYTES` unparsed and any object that repeats a key (`src/contract/strict.rs`); its callers still read untrusted input through a byte limit and act only on the returned `Value`.
- `PluginsConfig` (`FromStr`; `assay`, `binary_globs`, `plugins`), `AssaySettings` and its enums, `Plugin`, `Adapter` (`Temper`, `WeighIn`, `Exec`), `Step`, `Argv`, `GateName`, `Pin`, `CommitId`, `Sha256Digest`, `ExactVersion`, `DEFAULT_TIMEOUT_SECS`, `MAX_TIMEOUT_SECS`: base `.assay/plugins.toml`, parsed in-process (`src/config.rs`).
- The `GateRunner` port (`run(&mut self, &Plugin, GateJob) -> Result<GateRun, Self::Error>`), `GateRun`, `Termination`, `GateDocument`, `GateJob`, `BuildJob`, and the use-case functions `run_gates` and `account_for_required`, which produce `GateReport`s with a `GateOutcome` (`Concluded(Conclusion)` or `Error(GateError)`) (`src/gate.rs`).
- The `ModelRunner` port (03 §7, `src/model_runner.rs`): `run(&self, &ModelCall) -> Result<ModelResult, Self::Error>`, with `ModelCall`, `Prompt` (`InvalidPrompt`, `PromptError`), `ModelResult` (outcome, cost in `Aeu`, rate limits), `ModelOutcome` (`Ok(Structured)`, `BudgetExhausted`, `TurnsExhausted`, `Timeout`, `QuotaExhausted`, `Malformed`), `RateLimitObserved` (whose status is the domain's `RateLimitStatus`); and the retry policy `run_with_retry` (`ModelAttempts`, `RetryError`).
- The reviewer prompt bundle (04 §3, `src/prompt_bundle.rs`): `PromptBundle` (`new`, `hash`, `system_prompt`, `instruction`, `user_prompt`) over `BundleFiles` read by the caller from the paths `BundleFile::path` names under `plugin/prompts/`, refused with `InvalidBundle` (`BundleFileError::{Empty, Nul}`); `Role` (`Combined`, `CombinedMicro`) with the roster's `max_turns`, `cap` and `severity_ceiling`.

## Invariants
- Ports speak domain types; no adapter type crosses a port signature (ARC-03).
- A contract document is accepted only at a supported version and only if it satisfies that version's schema; unknown fields, unknown versions, over-length text, documents over `MAX_DOCUMENT_BYTES` and repeated object keys are rejected, never repaired (03 preamble).
- plugins.toml accepts no unknown key (there is no `applies_when`); pins and digests are immutable identities (`git:<40 hex>`, `oci:sha256:<64 hex>`), and a tag is `ParsePinError::Tag`; `exec` gates and declared steps name a Z2b job (`lint`, `test`), so nothing PR-controlled can be scheduled into `gates-static`; every timeout is 1 to 21,600 s.
- A gate run counts only as exit 0 (the document's conclusion) or exit 1 (fail) with a schema-valid gate document whose `source.name` is the plugin; anything else is `GateOutcome::Error`, displayed `gate-error:<detail>` (03 §2). `run_gates` has no filter, and `account_for_required` makes a required gate without a report `gate-error:skipped`.
- A model call is retried only when its first attempt is `Malformed`, and only once; every attempt's cost is reported, including a malformed first attempt whose retry failed (`RetryError::Retry`).
- Every role gets the same system prompt (one file, so the prompt cache is shared), and its user prompt is the context pack, a blank line, then its instruction. The bundle hash is the domain's `ContentHash` over every bundle file under its versioned path, so it changes when any byte, name or version does.
- Validation errors never quote the rejected value (it is untrusted text), nor an object key the schema does not name: such keys are reported as `*`, and unexpected properties are counted, not listed (`src/contract/violation.rs`).

## Errors
- `ReadContractError` (`TooLarge`, `Malformed` (including a repeated key), `MissingSchema`, `UnknownSchema`, `UnknownVersion`, `Invalid`): every variant maps to disposition `schema` through `disposition()`.
- `ContractSchemaError`: an embedded schema does not compile, a defect in the build. Its cause is the opaque `SchemaCompileError`, so `jsonschema`'s error type is not part of this crate's API.
- `ConfigError` (`Syntax` with the opaque `TomlError`, `Digest`, `Version`, `Pin` with `ParsePinError::{Tag, UnknownScheme}`, `Name`, `DuplicateName`, `UnknownAdapter`, `MissingKey`, `UnexpectedKey`, `Job`, `Command`, `Timeout`): plugins.toml was refused. The file is from the protected base, so messages may quote it.
- `GateError` is an outcome, not a failure of the use case: a `GateRunner` error becomes `GateError::NotRun`, with the runner's message kept in `GateReport::cause`.
- Use cases to come: one `thiserror` enum each; port failures surface through each port's associated error type.

## Test strategy
Integration tests in `tests/it/` drive use cases against in-memory fakes of the ports (TST-09); no real network, processes or clocks. Run: `cargo test -p assay-app`.

## Do not
- Do not call `gh`, `claude` or the file system directly; add or extend a port.
- Do not read environment variables; configuration arrives parsed from the CLI (ARC-11).

## Decisions
| Date | Decision | Rule | Decided by |
|---|---|---|---|
| 2026-10-11 | Crate created empty (Story #46); the edge to `assay-domain` is added when first used. | ARC-12, CRF-05 | Story #46 |
| 2026-10-11 | Contract validation lives here, not in the domain (whose dependencies are fixed to serde, thiserror and sha2) and not in an adapter (it does no I/O and every adapter needs it). Adds `assay-domain` (for `Disposition`), `jsonschema` (no default features: no HTTP or file resolution), `serde_json` and `thiserror`, all pre-approved. | ARC-02, ARC-03 | Story #47 |
| 2026-10-11 | plugins.toml parsing and the gate contract live here: parsing is pure (the text arrives from the caller), the domain may not take `toml`, and the classification needs `ContractValidator`. Adds `serde` and `toml`, both pre-approved. | ARC-02, ARC-03 | Story #62 |
| 2026-10-11 | `ModelRunner` (#82) differs from the 03 §7 sketch: `run` borrows the call (the retry reuses it), failures use an associated error type, `preflight` is left to #83, the role is left to #48, and the tool set is not a field (the adapter fixes it). | OWN-01, ARC-03 | Weigh-In default wi-696446 (two-way), owner may reopen |
| 2026-10-12 | Prompt bundle v1 (#90): the caller reads the files (no file I/O here) and the hash is the domain's (no `sha2` here). Role limits are code, not bundle data: they are 04 §3 policy, and a change to them changes the assay digest, which is in the evidence key too. | ARC-02, ARC-03 | #90 |
| 2026-10-11 | `ContractValidator` is a value built once by the composition root, not a lazily initialised static: no hidden global state, and the fallible compile step surfaces as a typed error. | OWN-11, ERR-01 | Story #47 |
