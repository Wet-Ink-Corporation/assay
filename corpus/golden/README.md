# corpus/golden/

The golden decision corpus (`docs/architecture/02-architecture.md` §7, issue #51): one directory per case, each holding

- `inputs.json`: the `DecisionInputs` of `crates/assay-domain` (`src/decide/inputs.rs`) in their serde form, and
- `decision.json`: the `assay.decision/1` that `decide` returns for them with `rubric/v1.toml`, written as pretty JSON with a final newline.

`crates/assay-domain/tests/it/golden.rs` decides every case and compares the result byte for byte with `decision.json`; it also checks that the corpus covers every reason the rubric can give. `crates/assay-app/tests/it/domain_model.rs` checks that every `decision.json` passes `schemas/decision-1.schema.json`. The `assay-self` gate (`assay self-gate corpus/golden rubric/v1.toml`, 02 §7, #76) replays the same corpus at a PR's head and fails on any byte difference; a case directory holds exactly these two regular files, and anything else under `corpus/golden/` but this README fails the gate too.

A byte difference is a verdict change. Changing a recorded `decision.json`, `rubric/v1.toml` or the rules is an owner decision (H-16, `05` T16), made in the same PR as the change that causes it: `corpus/golden/**` is a trust path of this repository (`.assay/globs.toml`), so the updated snapshot needs owner approval (rubric rule 4). `inputs_digest` covers the rubric and every input field, so a change to either changes every case's digest.

The inputs are a decide-specific projection, not a versioned contract, and have no JSON Schema.
