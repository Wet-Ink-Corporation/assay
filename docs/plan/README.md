# assay v1 plan helpers

The v1 plan lives as GitHub issues #1–#118 (Initiative #1, release Epics #2–#8). These one-off scripts finish what the GitHub tools used to create the issues could not do:

- `apply-labels.sh` creates the 17 plan labels and applies them to each issue.
- `apply-dependencies.sh` adds GitHub's native "blocked by" links that mirror each issue's "Blocked by" checklist (119 links).

Both need `gh` authenticated with write access to `Wet-Ink-Corporation/assay`, and both are safe to re-run.
