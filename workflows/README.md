# workflows/

Workflow templates installed into consumer repositories: `assay.yml.tmpl` and `assay-decide.yml.tmpl` (`docs/architecture/02-architecture.md` §7, `05-security.md`). Every action in them is pinned by full commit SHA. assay's own CI is `.github/workflows/rust.yml`, not these.

- `assay.yml.tmpl` (#60 ST-15): the pipeline, installed as `.github/workflows/assay.yml` on the default branch. Its header lists the environment, secrets and variables it expects. `crates/assay-cli/tests/it/pipeline_template.rs` pins its triggers, the guard before every secret (0006), the run-attempt guard, permissions, pins, the bundle flow and its argv. The argv comes from the table in `tests/it/pipeline_argv.rs`, which is also run against the binary.
- `assay-decide.yml.tmpl`: decide-only (#61), not written yet.
