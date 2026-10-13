# plugin/prompts/

Reviewer prompt bundles, content-addressed and versioned (`docs/architecture/02-architecture.md` §2, `04-pipeline.md` §3). CI passes the system prompt with `--append-system-prompt-file` from the base checkout.

## Layout

One directory per bundle version. `v1/` holds:

| File | Sent as | Used by |
|---|---|---|
| `system.md` | the system prompt, identical for every role so the prompt cache is shared | every role |
| `bugs.md` | the role instruction, after the context pack in the user prompt | `bugs` (full tier: 25 turns, $0.60 AEU) |
| `security.md` | the role instruction, after the context pack in the user prompt | `security` (full tier: 20 turns, $0.50 AEU) |
| `design.md` | the role instruction, after the context pack and the intent envelope in the user prompt | `design` (full tier: 20 turns, $0.50 AEU) |
| `meta.md` | the role instruction, after the context pack and the intent envelope in the user prompt | `meta` (full tier on trust-path or exec-surface PRs: 15 turns, $0.30 AEU) |
| `combined.md` | the role instruction, after the context pack in the user prompt | `combined` (lite tier: 20 turns, $0.50 AEU) |
| `combined-micro.md` | the role instruction, after the context pack in the user prompt | `combined-micro` (micro tier: 8 turns, $0.25 AEU, findings capped at `major`) |
| `verifier.md` | the blind verifier's instruction, after the one claim it judges (a JSON object of `file`, `line`, `class`, `claim`, `trigger`) | the blind verifier (lite tier: at most 3 calls, 8 turns and $0.08 AEU each; `assay_app::verify`) |
| `coordinator.md` | the coordinator's instruction, after the findings it groups (a JSON object of `findings`, each with an `id`) | the coordinator (full tier: 10 turns, $0.30 AEU for the stage; `assay_app::coordinate`) |

The system prompt carries the operational severity definitions of 04 §3 word for word (`crates/assay-app/tests/it/prompt_bundle.rs` checks them against 04) and its "what not to flag" list. Each role instruction is the last section of the user prompt and opens with `## Your role: <role>`; only `design` and `meta` are sent the pull request's intent text, inside its untrusted envelope, between the pack and the instruction (`assay_app::run_specialists`). The turn limits, caps and the severity ceiling are code (`assay_app::Role`, and the verifier's and coordinator's constants), not bundle data. The verifier's structured output follows `assay_app::VERDICT_SCHEMA` and the coordinator's `assay_app::COORDINATION_SCHEMA`, not the findings schema.

## Hash

The bundle hash is SHA-256 over every file of the bundle, in the order above, each as its path relative to this directory (`v1/system.md`) and its bytes, both prefixed by their length as a big-endian `u64` (`assay_domain::ContentHash`). Any changed byte, renamed file or new version changes the hash, and with it the evidence key (02 §3), so no evidence produced under other prompts is reused. This README is not part of any bundle.

## Changing a prompt

Edit the files of the current version when the bundle's files and roles stay the same; add a new version directory when they change. Exception: until a release has used a version, the roles 04 §3 lists join it as their stories land (`verifier.md` joined v1 with #91, `bugs.md`, `security.md`, `design.md` and `meta.md` with #95, and `coordinator.md` with #96). Prompt PRs run the corpus replay gate (04 §8) once M5 exists (see the root `AGENTS.md`).
