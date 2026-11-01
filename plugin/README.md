# plugin/

The assay Claude Code plugin (`docs/architecture/02-architecture.md` §2, §7), named `assay` by `.claude-plugin/plugin.json`:

- `commands/`: `/assay:check`, `/assay:replay`, `/assay:report`.
- `prompts/`: reviewer bundles, content-addressed and versioned.
- `hooks/`: the git pre-push helper (`pre-push`, #108).

This plugin ships no mod. The assay mod, an in-session view over `assay status` that is never a gate or a trust anchor (ADR-25, `docs/design/assay-mod.md`), is its own plugin, `assay-mod`, in `plugin-mod/` (#127): the desktop app's Chat/Cowork switches off a whole plugin that ships a mod, and would take these commands with it. Do not add `modules` to a `hooks/hooks.json` here.

In CI, prompts are delivered with `--append-system-prompt-file` from the base checkout, not with `--plugin-dir`. Everything here is a trust path (02 §7).
