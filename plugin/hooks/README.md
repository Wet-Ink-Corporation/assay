# plugin/hooks/

The pre-push helper hook of the assay plugin (`docs/architecture/02-architecture.md` §2, §6). Local mode is advisory: CI never trusts its output (ADR-10). It will also hold the assay mod, `register.tsx`: an in-session view over `assay.status/1` and `assay.event/1` with owner actions that only post `/assay` comments (ADR-25, `docs/design/assay-mod.md`). Empty until the local-mode and mod stories.
