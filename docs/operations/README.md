# Operations runbooks

Runbooks for operating assay. The design they implement is `docs/architecture/06-operations.md`.

| Runbook | Use it when |
|---|---|
| [break-glass.md](break-glass.md) | assay cannot produce a verdict at all and a PR must merge through the ruleset bypass |
| [release.md](release.md) | cutting an assay release and pinning its digest |
| [token-rotation.md](token-rotation.md) | the Claude OAuth token is expiring, expired or disclosed |
| [warm-target.md](warm-target.md) | operating the warm `target/` artifact |
