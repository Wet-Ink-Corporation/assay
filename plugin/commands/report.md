---
description: Show assay's periodic report on review cost, precision and overrides (arrives with #113)
argument-hint: "[--since <duration>]"
---

`/assay:report` will run `assay report $ARGUMENTS` (`--since 7d` by default): tiers, AEU and
cache hits, latency, gate failure causes, precision per severity with Wilson intervals, the
verifier's reject rate, injection and egress hits, overrides, waivers and dismissals
(`docs/architecture/06-operations.md` §7).

The `assay report` command arrives with #113 and does not exist yet. Tell the user so, and do
not assemble the figures from other sources.
