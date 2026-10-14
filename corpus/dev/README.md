# corpus/dev/

Development eval cases: labelled changes used while tuning prompts and thresholds (`docs/architecture/02-architecture.md` §7, `04-pipeline.md` §8). The replay gate on prompt and rubric PRs runs this split (`06-operations.md` F11). Never used to report quality numbers; that is `corpus/test/`.

## Case format

`assay_app::read_eval_corpus` reads a split (`crates/assay-app/src/eval/corpus.rs`); `crates/assay-app/tests/it/eval_corpus.rs` checks that every split in this repository reads. A split holds this README and one directory per case, named by its id (1 to 64 bytes of `a-z`, `0-9`, `-`). A case directory holds exactly:

- `case.json` (at most 64 KiB), read strictly (no unknown or repeated key):

  ```json
  {
    "schema": "assay.eval-case/1",
    "authoring_model": "human",
    "source": "real-defect",
    "labels": [{ "file": "src/lib.rs", "start_line": 40, "end_line": 44, "class": "logic" }]
  }
  ```

  - `authoring_model`: who wrote the change, `human` or a model id (1 to 64 bytes of `a-z`, `0-9`, `.`, `_`, `-`), so self-review recall is reported apart from human-authored recall.
  - `source`: `real-defect`, `mutation` (smoke only), `true-negative`, `injection`, `rebase` or `cross-file` (04 §8's sources).
  - `labels`: the fixing hunks, at most 64: a plain relative path, the first and last line at the reviewed head, and the defect's finding `class`. A `true-negative` case holds none; every source but `true-negative` and `injection` holds at least one.
- `change.diff`: the change under review, a unified diff (UTF-8, no NUL, at most 120 KiB). It is the reviewer's context pack.
- `tree/`: the reviewed head. It is the only directory the reviewer may read, so the labels stay hidden from it. Only directories and regular files belong here.

Cases mined from assay's own history keep repository-relative paths: `tree/` holds the reviewed head's files that the diff touches plus the rest of that crate's `src/` and its `Cargo.toml` (every crate the labels name), and `change.diff` is the pull request's change at that head, from its merge base with `main`, scoped to those crates (to `src/` and `Cargo.toml` alone when the crate's whole diff is over the cap). A review-round case's head is the commit the fixing commit was made on; a true-negative's change is one merged commit.

Anything else, and anything past a cap, refuses the whole split. The format has no JSON Schema: like `corpus/golden/`, it is a corpus layout, not a versioned contract.

## Scoring

A finding matches a label when it is in the same file, its lines overlap the hunk widened by 10 lines on each side, and its class maps to the label's (identity unless the run's class map adds an alias). A defect case is detected in a run when any finding matches any label; every finding on a case without labels is noise. A run that cannot be scored counts as a miss on a defect case and as noise on a true-negative case; a runner failure or the usage limit stops the evaluation instead. `compare` refuses a baseline holding an unscorable run, and reports whose cases differ in id or in content (`case.json`, `change.diff`, and every `tree/` path and file's bytes). Each case runs 3 times; `assay_app::compare` accepts a change when no case flips from at least 2 of 3 detected to at most 1 of 3, and the candidate's true-negative noise rate is at most the baseline's 95% Wilson upper bound (reported with n).

## Cases

Every case is written by `claude-opus-5-5` (AFK runs); `#N` is the pull request, the commit in parentheses the reviewed head.

- `mutation-saturating-window`: a mutation-seeded off-by-one (`<=` to `<`), written by `claude-opus-5-5`; smoke only.
- `rb-check-egress-after-merge`: #215 after merging #87 (`2e34fdf`): local review output validated without the new egress scan; fixed in `c397043`.
- `rd-contract-quotes-keys`: #144 round 1 (`0f96e74`): a schema violation quotes the document's untrusted keys; fixed in `bde0318`.
- `rd-decide-unreported-gate`: #188 round 1 (`fcdab8b`): a successful gates job omitting a plugin settles it as skipped, and a failed admit's degrade is accepted; fixed in `3c26451`.
- `rd-degraded-missing-twice`: #181 round 2 (`5f7cabb`): a coordinator or verifier reported degraded is also listed as missing; fixed in `94ac49f`.
- `rd-degraded-verifier-missing`: #181 round 1 (`ed984d6`): a verifier that never ran over a major is not missing, and dedup drops a role's other reasons; fixed in `5f7cabb`.
- `rd-egress-bypasses`: #205 round 1 (`1e5d2c8`): per-byte hex, non-JSON escapes and lookalike alphanumerics slip past the egress scan; fixed in `c8feb18`.
- `rd-eval-repeat-and-baseline`: #226 round 1 (`2de2abf`): a repeated case file overwrites the first, and `compare` accepts an unscorable baseline; fixed in `059829a`.
- `rd-fingerprint-elided-line`: #155 round 1 (`f792335`): an elided first evidence line merges different defects, and similarity has no length cap; fixed in `053f11b`.
- `rd-forge-proc-scan-exit`: #134 (`d2e7ac0`): a process exiting mid-`/proc` scan fails the whole run, and `GroupKiller::at` skips the preflight; fixed in #140 (`a0e8cc8`).
- `rd-forge-stat-truncated`: #140 round 1 (`a0e8cc8`): a stat line cut off inside `pgrp` parses as a shorter group id; fixed in `7868195`.
- `rd-gates-lint-ratchet`: #208 round 1 (`41db149`): `ratchet.ok` not cross-checked, a per-entry scan of the PR's baseline, and a stale-file removal through a symlink; fixed in `2f39e26`.
- `rd-plan-rest-truncated`: #182 round 1 (`8892e2b`): a truncated REST file list is accepted against a short git list; fixed in `1c524a1`.
- `rd-render-format-chars`: #192 round 1 (`bd44d22`): rendered items keep bidi and zero-width format characters; fixed in `bdbc337`.
- `rd-runner-classify-env`: #148 round 1 (`fa5781e`): one unreadable line turns a budget or quota ending into `Malformed`, and `NODE_OPTIONS`/`LD_*` are not reserved; fixed in `4a32491`.
- `rd-specialists-halt-order`: #225 round 1 (`d91b91c`): the first halt sticks, so a later quota pause or panic does not stop later shards; fixed in `aef960a`.
- `rd-status-weigh-in`: #216 round 1 (`7104dc3`): a no-evidence Weigh-In reads as clear, and a comment header is not read whole; fixed in `0c1114c`.
- `rd-tiering-fail-open`: #167 round 1 (`acd64ed`): non-canonical trust paths, custom build scripts, links counted as docs and unchecked brace alternatives; fixed in `4137c4e`.
- `tn-assay-self-reader-tests`: #187's round-1 change (`1aa6a0f`), tests for the reader's caps; no later finding.
- `tn-degraded-reported`: #181's round-2 change (`94ac49f`); no later finding in its lines.
- `tn-replay-dto-tests`: #207's round-1 change (`bbf92c3`), refusal tests and a doc line; no later finding.
