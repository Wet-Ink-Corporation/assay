---
description: Replay a recorded assay decision from its persisted input bundle (not wired yet)
argument-hint: "<pr-or-run>"
---

`/assay:replay` will run `assay replay --verify`: it fetches a run's persisted input bundle
(`assay.run-inputs/1`), recomputes the decision with `decide` and reports any byte difference
from the recorded one (R-09, ADR-15, `docs/architecture/04-pipeline.md` §8). An expired or
missing bundle is an error, never a pass.

The use case exists (`assay_app::replay_decision`, #74), but the `assay` binary has no `replay`
command yet: fetching a bundle needs the GitHub adapter (#53). Tell the user so, and do not
try to recompute a decision by hand or from PR comments.
