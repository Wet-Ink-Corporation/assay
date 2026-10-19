# plugin/hooks/

The hooks of the assay plugin (`docs/architecture/02-architecture.md` §2, §6). Local mode is advisory: CI never trusts its output (ADR-10).

- `pre-push`: the git pre-push helper (#108, `04-pipeline.md` §9 CP1). It runs `assay pre-push`, which lists the current branch's open Weigh-In items with `weighin open --repo --json`: a *blocking* item (open, triaged, and `door != two-way`, `authority == authority` or a non-null `blocks`) stops the push, an item *pending triage* only warns. `git push --no-verify` skips it. Install it with `ln -s /path/to/assay/plugin/hooks/pre-push .git/hooks/pre-push`, or `git config core.hooksPath /path/to/assay/plugin/hooks`. `ASSAY` names the `assay` program and `ASSAY_WEIGHIN` the `weighin` program (absolute; the plugin's launcher, `<weigh-in plugin>/libexec/weighin`, is not on `PATH`); with no `weighin`, or on a detached `HEAD`, it says it did not check and lets the push through. `crates/assay-cli/tests/it/pre_push.rs` pushes through it.
- The assay mod, `register.tsx` (later): an in-session view over `assay.status/1` and `assay.event/1` with owner actions that only post `/assay` comments (ADR-25, `docs/design/assay-mod.md`).
