//! Reading one eval corpus split (04 §8): the layout, the case format and every cap.

use std::error::Error;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use assay_app::{
    AuthoringModel, CaseProblem, CaseSource, CorpusEntry, CorpusEntryKind, CorpusError,
    MAX_AUTHORING_MODEL_BYTES, MAX_CASE_BYTES, MAX_CASE_ID_BYTES, MAX_DIFF_BYTES, MAX_EVAL_CASES,
    MAX_EVAL_ENTRIES, MAX_LABEL_PATH_BYTES, MAX_LABELS, Split, read_eval_corpus,
};
use assay_domain::Class;
use serde_json::json;

use crate::eval_fakes::{DEFECT_JSON, DIFF, QUIET_JSON, case_entries, dir, file};

type TestResult = Result<(), Box<dyn Error>>;

fn read(entries: &[CorpusEntry]) -> Result<assay_app::EvalCorpus, CorpusError> {
    read_eval_corpus(Split::Dev, entries)
}

/// `case.json` for a real defect with the given authoring model and labels.
fn case_json(model: &str, source: &str, labels: &serde_json::Value) -> String {
    json!({
        "schema": "assay.eval-case/1",
        "authoring_model": model,
        "source": source,
        "labels": labels,
    })
    .to_string()
}

fn label(file: &str, start: u64, end: u64) -> serde_json::Value {
    json!({"file": file, "start_line": start, "end_line": end, "class": "logic"})
}

fn refusal(json: &str) -> Result<CaseProblem, Box<dyn Error>> {
    match read(&case_entries("a-case", json, DIFF)) {
        Err(CorpusError::Case { case, problem }) if case == "a-case" => Ok(problem),
        other => Err(format!("expected a refused case.json, got {other:?}").into()),
    }
}

#[test]
fn reads_a_case_with_its_author_source_labels_and_diff() -> TestResult {
    let mut entries = vec![file("README.md", b"# dev\n")];
    entries.extend(case_entries("off-by-one", DEFECT_JSON, DIFF));

    let corpus = read(&entries)?;

    let [case] = corpus.cases() else {
        return Err("one case expected".into());
    };
    assert_eq!(case.id(), "off-by-one");
    assert_eq!(case.authoring_model(), &AuthoringModel::Human);
    assert_eq!(case.source(), CaseSource::RealDefect);
    assert_eq!(case.diff(), DIFF);
    let [hunk] = case.labels() else {
        return Err("one label expected".into());
    };
    assert_eq!(
        (
            hunk.file(),
            hunk.first().get(),
            hunk.last().get(),
            hunk.class()
        ),
        ("src/lib.rs", 40, 44, Class::Logic)
    );
    Ok(())
}

#[test]
fn reads_cases_in_id_order_with_a_model_author() -> TestResult {
    let mut entries = case_entries("zeta", DEFECT_JSON, DIFF);
    entries.extend(case_entries("alpha", QUIET_JSON, DIFF));

    let corpus = read(&entries)?;

    let ids: Vec<&str> = corpus.cases().iter().map(assay_app::EvalCase::id).collect();
    assert_eq!(ids, ["alpha", "zeta"]);
    assert_eq!(
        corpus
            .cases()
            .first()
            .map(assay_app::EvalCase::authoring_model),
        Some(&AuthoringModel::Model("claude-opus-5-5".to_owned()))
    );
    Ok(())
}

#[test]
fn an_empty_split_reads_as_no_case() -> TestResult {
    let corpus = read(&[file("README.md", b"# dev\n")])?;

    assert!(corpus.cases().is_empty(), "{corpus:?}");
    Ok(())
}

#[test]
fn refuses_a_repeated_key() -> TestResult {
    let json = r#"{"schema": "assay.eval-case/1", "schema": "assay.eval-case/1",
        "authoring_model": "human", "source": "true-negative", "labels": []}"#;

    assert_eq!(refusal(json)?, CaseProblem::Syntax { line: 1 });
    Ok(())
}

#[test]
fn refuses_an_unknown_field() -> TestResult {
    let json = r#"{"schema": "assay.eval-case/1", "authoring_model": "human",
        "source": "true-negative", "labels": [], "notes": "x"}"#;

    assert!(
        matches!(refusal(json)?, CaseProblem::Syntax { .. }),
        "unknown field"
    );
    Ok(())
}

#[test]
fn refuses_an_unknown_class_in_a_label() -> TestResult {
    let labels = json!([{"file": "a.rs", "start_line": 1, "end_line": 1, "class": "style"}]);

    let problem = refusal(&case_json("human", "real-defect", &labels))?;

    assert!(matches!(problem, CaseProblem::Syntax { .. }), "{problem:?}");
    Ok(())
}

#[test]
fn refuses_another_schema() -> TestResult {
    let json = DEFECT_JSON.replace("assay.eval-case/1", "assay.eval-case/2");

    assert_eq!(refusal(&json)?, CaseProblem::Schema);
    Ok(())
}

#[test]
fn accepts_a_model_id_at_the_length_cap() -> TestResult {
    let model = "m".repeat(MAX_AUTHORING_MODEL_BYTES);
    let json = case_json(&model, "true-negative", &json!([]));

    let corpus = read(&case_entries("a-case", &json, DIFF))?;

    let authors: Vec<_> = corpus
        .cases()
        .iter()
        .map(|case| case.authoring_model().clone())
        .collect();
    assert_eq!(authors, [AuthoringModel::Model(model)]);
    Ok(())
}

#[test]
fn refuses_a_model_id_past_the_length_cap() -> TestResult {
    let model = "m".repeat(MAX_AUTHORING_MODEL_BYTES + 1);

    let problem = refusal(&case_json(&model, "true-negative", &json!([])))?;

    assert_eq!(problem, CaseProblem::AuthoringModel);
    Ok(())
}

#[test]
fn refuses_an_empty_or_unshaped_model_id() -> TestResult {
    for model in ["", "Claude", "claude opus", "claude/opus"] {
        let problem = refusal(&case_json(model, "true-negative", &json!([])))?;

        assert_eq!(problem, CaseProblem::AuthoringModel, "{model:?}");
    }
    Ok(())
}

#[test]
fn refuses_a_label_on_a_true_negative() -> TestResult {
    let problem = refusal(&case_json(
        "human",
        "true-negative",
        &json!([label("a.rs", 1, 1)]),
    ))?;

    assert_eq!(problem, CaseProblem::LabelsOnTrueNegative);
    Ok(())
}

#[test]
fn refuses_a_defect_case_without_a_label() -> TestResult {
    for source in ["real-defect", "mutation", "rebase", "cross-file"] {
        let problem = refusal(&case_json("human", source, &json!([])))?;

        assert_eq!(problem, CaseProblem::NoLabels, "{source}");
    }
    Ok(())
}

#[test]
fn an_injection_case_may_hold_no_label() -> TestResult {
    let json = case_json("human", "injection", &json!([]));

    let corpus = read(&case_entries("hostile", &json, DIFF))?;

    let sources: Vec<_> = corpus
        .cases()
        .iter()
        .map(assay_app::EvalCase::source)
        .collect();
    assert_eq!(sources, [CaseSource::Injection]);
    Ok(())
}

#[test]
fn refuses_a_label_that_ends_before_it_starts() -> TestResult {
    let labels = json!([label("a.rs", 1, 1), label("a.rs", 9, 8)]);

    let problem = refusal(&case_json("human", "real-defect", &labels))?;

    assert_eq!(problem, CaseProblem::LabelSpan { index: 1 });
    Ok(())
}

#[test]
fn refuses_a_label_path_that_is_not_plain_and_relative() -> TestResult {
    let too_long = "a".repeat(MAX_LABEL_PATH_BYTES + 1);
    for path in [
        "",
        "/etc/passwd",
        "../x.rs",
        "src/./x.rs",
        "src//x.rs",
        too_long.as_str(),
    ] {
        let labels = json!([label(path, 1, 1)]);

        let problem = refusal(&case_json("human", "real-defect", &labels))?;

        assert_eq!(problem, CaseProblem::LabelPath { index: 0 }, "{path:?}");
    }
    Ok(())
}

#[test]
fn accepts_a_label_path_at_the_length_cap() -> TestResult {
    let path = "a".repeat(MAX_LABEL_PATH_BYTES);
    let json = case_json("human", "real-defect", &json!([label(&path, 1, 1)]));

    let corpus = read(&case_entries("a-case", &json, DIFF))?;

    let files: Vec<&str> = corpus
        .cases()
        .iter()
        .flat_map(assay_app::EvalCase::labels)
        .map(assay_app::Label::file)
        .collect();
    assert_eq!(files, [path.as_str()]);
    Ok(())
}

#[test]
fn accepts_labels_up_to_the_cap() -> TestResult {
    let cap = u64::try_from(MAX_LABELS)?;
    let labels: Vec<_> = (1..=cap).map(|line| label("a.rs", line, line)).collect();
    let json = case_json("human", "real-defect", &json!(labels));

    let corpus = read(&case_entries("a-case", &json, DIFF))?;

    let count: usize = corpus.cases().iter().map(|case| case.labels().len()).sum();
    assert_eq!(count, MAX_LABELS);
    Ok(())
}

#[test]
fn refuses_one_label_past_the_cap() -> TestResult {
    let over = u64::try_from(MAX_LABELS)? + 1;
    let labels: Vec<_> = (1..=over).map(|line| label("a.rs", line, line)).collect();

    let problem = refusal(&case_json("human", "real-defect", &json!(labels)))?;

    assert_eq!(problem, CaseProblem::TooManyLabels);
    Ok(())
}

/// `json` padded with trailing spaces to exactly `len` bytes (JSON allows trailing space).
fn padded(json: &str, len: usize) -> String {
    format!("{json}{}", " ".repeat(len - json.len()))
}

#[test]
fn accepts_a_case_file_at_the_byte_cap() -> TestResult {
    let json = padded(QUIET_JSON, MAX_CASE_BYTES);

    let corpus = read(&case_entries("a-case", &json, DIFF))?;

    assert_eq!(corpus.cases().len(), 1);
    Ok(())
}

#[test]
fn refuses_a_case_file_one_byte_past_the_cap() {
    let json = padded(QUIET_JSON, MAX_CASE_BYTES + 1);

    let result = read(&case_entries("a-case", &json, DIFF));

    assert_eq!(
        result,
        Err(CorpusError::TooLarge {
            path: "a-case/case.json".to_owned(),
            limit: MAX_CASE_BYTES,
        })
    );
}

#[test]
fn accepts_a_diff_at_the_byte_cap() -> TestResult {
    let diff = "x".repeat(MAX_DIFF_BYTES);

    let corpus = read(&case_entries("a-case", QUIET_JSON, &diff))?;

    let lengths: Vec<usize> = corpus
        .cases()
        .iter()
        .map(|case| case.diff().len())
        .collect();
    assert_eq!(lengths, [MAX_DIFF_BYTES]);
    Ok(())
}

#[test]
fn refuses_a_diff_one_byte_past_the_cap() {
    let diff = "x".repeat(MAX_DIFF_BYTES + 1);

    let result = read(&case_entries("a-case", QUIET_JSON, &diff));

    assert_eq!(
        result,
        Err(CorpusError::TooLarge {
            path: "a-case/change.diff".to_owned(),
            limit: MAX_DIFF_BYTES,
        })
    );
}

#[test]
fn refuses_a_diff_that_is_not_text() {
    for diff in [&b"\xff\xfe"[..], b"a\0b"] {
        let mut entries = case_entries("a-case", QUIET_JSON, "");
        entries.retain(|entry| entry.path != "a-case/change.diff");
        entries.push(file("a-case/change.diff", diff));

        let result = read(&entries);

        assert_eq!(
            result,
            Err(CorpusError::DiffNotText {
                case: "a-case".to_owned()
            }),
            "{diff:?}"
        );
    }
}

#[test]
fn refuses_a_case_missing_any_of_its_three_entries() {
    for (missing, name) in [
        ("a-case/case.json", "case.json"),
        ("a-case/change.diff", "change.diff"),
        ("a-case/tree", "tree"),
    ] {
        let mut entries = case_entries("a-case", QUIET_JSON, DIFF);
        entries.retain(|entry| entry.path != missing && !entry.path.starts_with("a-case/tree/"));

        let result = read(&entries);

        assert_eq!(
            result,
            Err(CorpusError::Incomplete {
                case: "a-case".to_owned(),
                missing: name,
            }),
            "{missing}"
        );
    }
}

#[test]
fn refuses_the_first_unexpected_entry_in_path_order() {
    let mut entries = case_entries("a-case", QUIET_JSON, DIFF);
    entries.push(file("a-case/notes.md", b"x"));
    entries.push(file("a-case/labels.json", b"x"));

    let result = read(&entries);

    assert_eq!(
        result,
        Err(CorpusError::UnexpectedEntry {
            path: "a-case/labels.json".to_owned()
        })
    );
}

#[test]
fn refuses_entries_that_do_not_belong() {
    let unexpected = [
        file("notes.md", b"x"),
        dir("README.md"),
        dir("Upper-Case"),
        dir(&"a".repeat(MAX_CASE_ID_BYTES + 1)),
        dir("a-case/case.json"),
        dir("a-case/extra"),
        file("a-case/tree", b"x"),
        file("a-case/tree/../escape", b"x"),
        CorpusEntry {
            path: "a-case/tree/link".to_owned(),
            kind: CorpusEntryKind::Other,
        },
    ];
    for entry in unexpected {
        let mut entries = case_entries("a-case", QUIET_JSON, DIFF);
        let path = entry.path.clone();
        entries.push(entry);

        let result = read(&entries);

        assert_eq!(
            result,
            Err(CorpusError::UnexpectedEntry { path: path.clone() }),
            "{path}"
        );
    }
}

#[test]
fn accepts_a_case_id_at_the_length_cap() -> TestResult {
    let id = "a".repeat(MAX_CASE_ID_BYTES);

    let corpus = read(&case_entries(&id, QUIET_JSON, DIFF))?;

    let ids: Vec<&str> = corpus.cases().iter().map(assay_app::EvalCase::id).collect();
    assert_eq!(ids, [id.as_str()]);
    Ok(())
}

/// One true-negative case per id, listed as its three required entries.
fn many_cases(count: usize) -> Vec<CorpusEntry> {
    (0..count)
        .flat_map(|index| {
            let id = format!("case-{index}");
            [
                file(&format!("{id}/case.json"), QUIET_JSON.as_bytes()),
                file(&format!("{id}/change.diff"), DIFF.as_bytes()),
                dir(&format!("{id}/tree")),
            ]
        })
        .collect()
}

#[test]
fn accepts_as_many_cases_as_the_cap() -> TestResult {
    let corpus = read(&many_cases(MAX_EVAL_CASES))?;

    assert_eq!(corpus.cases().len(), MAX_EVAL_CASES);
    Ok(())
}

#[test]
fn refuses_one_case_past_the_cap() {
    let result = read(&many_cases(MAX_EVAL_CASES + 1));

    assert_eq!(
        result,
        Err(CorpusError::TooManyCases {
            limit: MAX_EVAL_CASES
        })
    );
}

/// `count` entries: one case and tree files to make up the rest.
fn many_entries(count: usize) -> Vec<CorpusEntry> {
    let mut entries = case_entries("a-case", QUIET_JSON, DIFF);
    let filler = (entries.len()..count).map(|index| dir(&format!("a-case/tree/d{index}")));
    entries.extend(filler);
    entries
}

#[test]
fn accepts_as_many_entries_as_the_cap() -> TestResult {
    let corpus = read(&many_entries(MAX_EVAL_ENTRIES))?;

    assert_eq!(corpus.cases().len(), 1);
    Ok(())
}

#[test]
fn refuses_one_entry_past_the_cap() {
    let result = read(&many_entries(MAX_EVAL_ENTRIES + 1));

    assert_eq!(
        result,
        Err(CorpusError::TooManyEntries {
            limit: MAX_EVAL_ENTRIES
        })
    );
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every entry under `dir`, paths relative to it; tree files are listed without their bytes.
fn listing(dir: &Path, prefix: &str, entries: &mut Vec<CorpusEntry>) -> TestResult {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("non-UTF-8 name {}", name.display()))?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            entries.push(CorpusEntry {
                path: path.clone(),
                kind: CorpusEntryKind::Directory,
            });
            listing(&entry.path(), &path, entries)?;
        } else if kind.is_file() {
            entries.push(CorpusEntry {
                kind: CorpusEntryKind::File(fs::read(entry.path())?),
                path,
            });
        } else {
            entries.push(CorpusEntry {
                path,
                kind: CorpusEntryKind::Other,
            });
        }
    }
    Ok(())
}

#[test]
fn every_split_in_the_repository_reads() -> TestResult {
    for split in [Split::Dev, Split::Test, Split::Inject] {
        let mut entries = Vec::new();
        listing(
            &repo_root().join("corpus").join(split.dir()),
            "",
            &mut entries,
        )?;

        let corpus = read_eval_corpus(split, &entries);

        assert!(
            matches!(corpus, Ok(ref read) if read.split() == split),
            "{split}: {corpus:?}"
        );
    }
    Ok(())
}

#[test]
fn the_repository_dev_split_holds_its_seed_case() -> TestResult {
    let mut entries = Vec::new();
    listing(&repo_root().join("corpus/dev"), "", &mut entries)?;

    let corpus = read_eval_corpus(Split::Dev, &entries)?;

    let first = corpus.cases().first().ok_or("no seed case")?;
    assert_eq!(first.id(), "mutation-saturating-window");
    assert_eq!(first.source(), CaseSource::Mutation);
    let hunk = first.labels().first().ok_or("no label")?;
    assert_eq!(hunk.first(), NonZeroU64::new(8).ok_or("zero")?);
    Ok(())
}
