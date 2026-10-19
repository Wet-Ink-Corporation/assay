# plugin/

The assay Claude Code plugin (`docs/architecture/02-architecture.md` §2, §7), named `assay` by `.claude-plugin/plugin.json`:

- `commands/`: `/assay:check`, `/assay:replay`, `/assay:report`.
- `prompts/`: reviewer bundles, content-addressed and versioned.
- `hooks/`: the git pre-push helper (`pre-push`, #108), and the assay mod (`hooks/register.tsx`, with `types/` and tests): an in-session view over `assay status`, never a gate or a trust anchor (ADR-25, `docs/design/assay-mod.md`).

In CI, prompts are delivered with `--append-system-prompt-file` from the base checkout, not with `--plugin-dir`. Everything here is a trust path (02 §7).
