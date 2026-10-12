# rubric/

The versioned review rubric (`docs/architecture/02-architecture.md` §7, `04-pipeline.md` §5).

- `v1.toml`: the data rubric v1's rules read: `version` (recorded as `rubric_version`), the rule 7 majors threshold, the rule 8a requeue limit, the micro roles, and the human-decision gate findings an owner may waive.

The rules themselves are the pure `decide` function in `crates/assay-domain` (`src/decide.rs`), which owns its meaning through the validated `Rubric` type; the domain reads no files, so the caller deserialises the TOML into `Rubric`. The golden decisions in `corpus/golden/` are made with `v1.toml`. This directory is a trust path.
