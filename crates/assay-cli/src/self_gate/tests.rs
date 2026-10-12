//! The reader's caps and refusals (H-13), and the variables `run` requires, on scratch trees.

use std::error::Error;

use tempfile::TempDir;

use super::*;

type TestResult = Result<(), Box<dyn Error>>;

/// A scratch directory holding `files` (relative path, bytes), parents created.
fn tree(files: &[(&str, &[u8])]) -> Result<TempDir, Box<dyn Error>> {
    let dir = TempDir::new()?;
    for (path, bytes) in files {
        let path = dir.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
    }
    Ok(dir)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs the gate over `corpus` with the repository's rubric, writing into `out`.
fn run_over(
    corpus: &Path,
    out: &Path,
    run_key: Option<OsString>,
) -> Result<GateConclusion, SelfGateError> {
    let args = [
        OsString::from(corpus),
        repo_root().join("rubric/v1.toml").into_os_string(),
    ];
    let variables = GateVariables {
        findings_out: Some(out.join("findings.json").into_os_string()),
        run_key,
    };
    run(args.into_iter(), variables)
}

fn run_key() -> OsString {
    OsString::from("sha256:unit")
}

#[test]
fn a_file_at_the_per_file_cap_is_read_and_one_byte_more_is_too_large() -> TestResult {
    let dir = tree(&[("at-cap", b"1234"), ("over-cap", b"12345")])?;
    let mut budget = Budget::with(10, 100, 4);

    let at_cap = read_bounded(&dir.path().join("at-cap"), &mut budget)?;
    let over_cap = read_bounded(&dir.path().join("over-cap"), &mut budget);

    assert_eq!(at_cap, b"1234", "exactly the cap is accepted");
    assert!(
        matches!(over_cap, Err(SelfGateError::TooLarge { .. })),
        "one byte over is refused: {over_cap:?}"
    );
    Ok(())
}

#[test]
fn the_entry_past_the_entry_cap_is_too_large() -> TestResult {
    let dir = tree(&[("a", b"a"), ("b", b"b"), ("c", b"c")])?;

    let within = read_corpus(dir.path(), &mut Budget::with(3, 100, 10))?;
    let past = read_corpus(dir.path(), &mut Budget::with(2, 100, 10));

    assert_eq!(within.len(), 3, "three entries fit a cap of three");
    assert!(
        matches!(past, Err(SelfGateError::TooLarge { .. })),
        "the third entry passes a cap of two: {past:?}"
    );
    Ok(())
}

#[test]
fn a_file_after_the_total_budget_is_spent_is_too_large() -> TestResult {
    let dir = tree(&[("a", b"123"), ("b", b"456")])?;

    let past = read_corpus(dir.path(), &mut Budget::with(10, 5, 10));

    assert!(
        matches!(past, Err(SelfGateError::TooLarge { .. })),
        "six bytes in all pass a total of five, each file under the per-file cap: {past:?}"
    );
    Ok(())
}

#[test]
fn the_rubric_is_refused_when_the_corpus_spent_the_budget() -> TestResult {
    let corpus = tree(&[("case/inputs.json", b"1234")])?;
    let rubric = tree(&[("v1.toml", b"12")])?;
    let mut budget = Budget::with(10, 5, 10);

    read_corpus(corpus.path(), &mut budget)?;
    let refused = read_file(&rubric.path().join("v1.toml"), &mut budget);

    assert!(
        matches!(refused, Err(SelfGateError::TooLarge { .. })),
        "one byte of budget is left for a two-byte rubric: {refused:?}"
    );
    Ok(())
}

#[test]
fn the_default_budget_is_the_documented_caps() {
    let budget = Budget::new();

    assert_eq!(
        (budget.entries, budget.bytes, budget.file_bytes),
        (4_096, 64 << 20, 1 << 20),
        "4,096 entries, 64 MiB in all, 1 MiB per file (02 §7)"
    );
}

#[cfg(unix)]
mod unix {
    use std::ffi::OsStr;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn a_missing_run_key_and_a_non_unicode_one_are_reported_apart() -> TestResult {
        let corpus = TempDir::new()?;
        let out = TempDir::new()?;
        let not_unicode = Some(OsString::from_vec(vec![0xff]));

        let missing = run_over(corpus.path(), out.path(), None);
        let not_unicode = run_over(corpus.path(), out.path(), not_unicode);

        assert!(
            matches!(missing, Err(SelfGateError::Variable(RUN_KEY))),
            "missing: {missing:?}"
        );
        assert!(
            matches!(not_unicode, Err(SelfGateError::VariableNotUnicode(RUN_KEY))),
            "not Unicode: {not_unicode:?}"
        );
        Ok(())
    }

    /// A corpus with the recorded `rule-9-success` case.
    fn one_case_corpus() -> Result<TempDir, Box<dyn Error>> {
        let case = repo_root().join("corpus/golden/rule-9-success");
        tree(&[
            (
                "rule-9-success/inputs.json",
                &fs::read(case.join("inputs.json"))?,
            ),
            (
                "rule-9-success/decision.json",
                &fs::read(case.join("decision.json"))?,
            ),
        ])
    }

    /// The rule of each finding in the document written to `out`.
    fn rules(out: &Path) -> Result<Vec<String>, Box<dyn Error>> {
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(out.join("findings.json"))?)?;
        let findings = document["findings"].as_array().ok_or("a findings array")?;
        Ok(findings
            .iter()
            .filter_map(|finding| finding["rule"].as_str().map(str::to_owned))
            .collect())
    }

    #[test]
    fn the_one_case_corpus_passes() -> TestResult {
        let corpus = one_case_corpus()?;
        let out = TempDir::new()?;

        let conclusion = run_over(corpus.path(), out.path(), Some(run_key()))?;

        assert_eq!(
            conclusion,
            GateConclusion::Pass,
            "the control for the symlink test"
        );
        Ok(())
    }

    #[test]
    fn a_symlinked_case_file_is_listed_unread_and_fails_the_replay() -> TestResult {
        let corpus = one_case_corpus()?;
        symlink(
            corpus.path().join("rule-9-success/inputs.json"),
            corpus.path().join("rule-9-success/link.json"),
        )?;
        let out = TempDir::new()?;

        let entries = read_corpus(corpus.path(), &mut Budget::new())?;
        let conclusion = run_over(corpus.path(), out.path(), Some(run_key()))?;

        let link = entries
            .iter()
            .find(|entry| entry.path == "rule-9-success/link.json")
            .ok_or("the symlink is listed")?;
        assert_eq!(link.kind, CorpusEntryKind::Other, "never followed or read");
        assert_eq!(
            (conclusion, rules(out.path())?),
            (
                GateConclusion::Fail,
                vec!["golden-unexpected-entry".to_owned()]
            ),
            "the replay refuses it"
        );
        Ok(())
    }

    #[test]
    fn a_symlinked_corpus_directory_is_the_wrong_shape() -> TestResult {
        let corpus = one_case_corpus()?;
        let links = TempDir::new()?;
        let link = links.path().join("corpus");
        symlink(corpus.path(), &link)?;

        let refused = read_corpus(&link, &mut Budget::new());

        assert!(
            matches!(
                refused,
                Err(SelfGateError::Shape {
                    expected: "a directory",
                    ..
                })
            ),
            "{refused:?}"
        );
        Ok(())
    }

    #[test]
    fn a_symlinked_rubric_is_the_wrong_shape() -> TestResult {
        let links = TempDir::new()?;
        let link = links.path().join("v1.toml");
        symlink(repo_root().join("rubric/v1.toml"), &link)?;

        let refused = read_file(&link, &mut Budget::new());

        assert!(
            matches!(
                refused,
                Err(SelfGateError::Shape {
                    expected: "a regular file",
                    ..
                })
            ),
            "{refused:?}"
        );
        Ok(())
    }

    #[test]
    fn a_non_utf8_file_name_is_the_wrong_shape() -> TestResult {
        let corpus = TempDir::new()?;
        fs::write(corpus.path().join(OsStr::from_bytes(b"case-\xff")), b"{}")?;

        let refused = read_corpus(corpus.path(), &mut Budget::new());

        assert!(
            matches!(
                refused,
                Err(SelfGateError::Shape {
                    expected: "a UTF-8 name",
                    ..
                })
            ),
            "{refused:?}"
        );
        Ok(())
    }
}
