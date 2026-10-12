# plugin/prompts/

Reviewer prompt bundles, content-addressed and versioned (`docs/architecture/02-architecture.md` §2, `04-pipeline.md` §3). CI passes the system prompt with `--append-system-prompt-file` from the base checkout.

## Layout

One directory per bundle version. `v1/` holds:

| File | Sent as | Used by |
|---|---|---|
| `system.md` | the system prompt, identical for every role so the prompt cache is shared | every role |
| `combined.md` | the role instruction, after the context pack in the user prompt | `combined` (lite tier: 20 turns, $0.50 AEU) |
| `combined-micro.md` | the role instruction, after the context pack in the user prompt | `combined-micro` (micro tier: 8 turns, $0.25 AEU, findings capped at `major`) |

The system prompt carries the operational severity definitions of 04 §3 word for word (`crates/assay-app/tests/it/prompt_bundle.rs` checks them against 04) and its "what not to flag" list. The turn limits, caps and the severity ceiling are code (`assay_app::Role`), not bundle data.

## Hash

The bundle hash is SHA-256 over every file of the bundle, in the order above, each as its path relative to this directory (`v1/system.md`) and its bytes, both prefixed by their length as a big-endian `u64` (`assay_domain::ContentHash`). Any changed byte, renamed file or new version changes the hash, and with it the evidence key (02 §3), so no evidence produced under other prompts is reused. This README is not part of any bundle.

## Changing a prompt

Edit the files of the current version when the bundle's files and roles stay the same; add a new version directory when they change. Prompt PRs run the corpus replay gate (04 §8) once M5 exists (see the root `AGENTS.md`).
