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

Anything else, and anything past a cap, refuses the whole split. The format has no JSON Schema: like `corpus/golden/`, it is a corpus layout, not a versioned contract.

## Scoring

A finding matches a label when it is in the same file, its lines overlap the hunk widened by 10 lines on each side, and its class maps to the label's (identity unless the run's class map adds an alias). A defect case is detected in a run when any finding matches any label; every finding on a case without labels is noise. A run that cannot be scored counts as a miss on a defect case and as noise on a true-negative case; a runner failure or the usage limit stops the evaluation instead. `compare` refuses a baseline holding an unscorable run, and reports whose cases differ in id or in `case.json`/`change.diff` content. Each case runs 3 times; `assay_app::compare` accepts a change when no case flips from at least 2 of 3 detected to at most 1 of 3, and the candidate's true-negative noise rate is at most the baseline's 95% Wilson upper bound (reported with n).

## Cases

- `mutation-saturating-window`: a mutation-seeded off-by-one (`<=` to `<`), written by `claude-opus-5-5`; smoke only.
