# corpus/test/

Held-out eval cases: labelled changes used only to measure review quality (`docs/architecture/02-architecture.md` §7, `04-pipeline.md` §8, `06-operations.md`). Never read during prompt iteration, never tuned against, and refreshed from new real defects each quarter. The case format is `corpus/dev/README.md`'s.

## Cases

Every case is written by `claude-opus-5-5` (AFK runs); `#N` is the pull request, the commit in parentheses the reviewed head.

- `rd-eval-digest-skips-tree`: #226 round 2 (`059829a`): the case digest leaves `tree/` out, and a repeated tree entry is accepted; fixed in `d81639d`.
- `rd-fingerprint-contentless`: #155 round 2 (`053f11b`): evidence or an expected behaviour without content still fingerprints and matches, and an omission without one anchors as a defect; fixed in `3bb2000`.
- `rd-forge-ancestry-broken-store`: #222 round 1 (`ada4a12`): any presence-check failure reads as unreachable, and range-diff runs unbounded; fixed in `ddae85b`.
- `rd-forge-stat-non-utf8`: #140 (`7868195`): `/proc/<pid>/stat` is read as UTF-8, so a non-UTF-8 `comm` fails every run; fixed in #142 (`5295e2d`).
- `rd-publish-gh-reference`: #200 round 1 (`c1ac6ed`): `GH-<n>` references stay live, and a path with a control character gets a permalink; fixed in `9829bd0`.
- `rd-runner-canary-sandbox`: #191 round 1 (`c626bd5`): a failed canary leaves its sandbox behind, and the init reader accepts a repeated key; fixed in `6f619a6`.
- `rd-waiver-unbound`: #162 round 1 (`d4f44fa`): a waiver clears findings by fingerprint alone, across gates and rules; fixed in `41fe2d2`.
- `tn-cli-coordinator-bundle`: #221's post-merge change (`d85e6d5`), the coordinator instruction in local mode's bundle; no later finding.
- `tn-forge-stat-bytes`: #142 (`5295e2d`), stat files read as bytes; no later finding.
- `xf-group-kill-own-group`: #157 round 1 (`32ce715`): the group kill may signal assay's own pid or group (forge), and `stop` scans `/proc` before killing an unreaped leader's group (runner); fixed in `0cbb1f7`.
