# plugin/

The assay Claude Code plugin (`docs/architecture/02-architecture.md` §2, §7):

- `commands/`: `/assay:check`, `/assay:replay`, `/assay:report`.
- `prompts/`: reviewer bundles, content-addressed and versioned.
- `hooks/`: the pre-push helper.

In CI, prompts are delivered with `--append-system-prompt-file` from the base checkout, not with `--plugin-dir`. Everything here is a trust path (02 §7).
