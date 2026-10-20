# plugin/commands/

Slash commands of the assay plugin (`docs/architecture/02-architecture.md` §2), named by `../.claude-plugin/plugin.json` (`assay`):

- `check.md`, `/assay:check`: runs `assay check --local`, the advisory check of the working tree, which ends with the Weigh-In checkpoint CP1 (`04-pipeline.md` §9).
- `replay.md`, `/assay:replay`: decision replay, `assay replay --verify` (R-09); the binary has no `replay` command until the GitHub adapter (#53) can fetch bundles, and the command says so.
- `report.md`, `/assay:report`: `assay report` (06 §7); the command arrives with #113, and until then the slash command says so.
