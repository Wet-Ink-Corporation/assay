//! The `assay-self` gate's replay (02 §7, 05 T16): `decide` over the golden corpus, byte for
//! byte against the recorded snapshots, as an `assay.findings/1` gate document.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use assay_app::{
    Contract, ContractValidator, CorpusEntry, CorpusEntryKind, GoldenReplay, SELF_GATE,
    replay_golden,
};
use assay_domain::{
    DecisionInputs, FindingsDocument, GateConclusion, Rubric, Source, SourceKind, decide,
};

type TestResult = Result<(), Box<dyn Error>>;

const CORPUS_DIR: &str = "corpus/golden";
const RUBRIC_PATH: &str = "rubric/v1.toml";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The recorded corpus as the binary lists it: every directory and file under it, sorted.
fn recorded_corpus() -> Result<Vec<CorpusEntry>, Box<dyn Error>> {
    let root = repo_root().join(CORPUS_DIR);
    let mut entries = Vec::new();
    collect(&root, "", &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn collect(dir: &Path, prefix: &str, entries: &mut Vec<CorpusEntry>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("non-UTF-8 name {}", name.display()))?;
        let path = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            collect(&entry.path(), &format!("{path}/"), entries)?;
            entries.push(CorpusEntry {
                path,
                kind: CorpusEntryKind::Directory,
            });
        } else {
            entries.push(CorpusEntry {
                path,
                kind: CorpusEntryKind::File(fs::read(entry.path())?),
            });
        }
    }
    Ok(())
}

fn recorded_rubric() -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(repo_root().join(RUBRIC_PATH))?)
}

/// `rubric/v1.toml` with its rule-7 threshold raised: a rubric change.
fn changed_rubric() -> Result<Vec<u8>, Box<dyn Error>> {
    let text = String::from_utf8(recorded_rubric()?)?;
    let line = text
        .lines()
        .find(|line| line.starts_with("majors_block_threshold"))
        .ok_or("rubric/v1.toml sets majors_block_threshold")?;
    Ok(text
        .replacen(line, "majors_block_threshold = 97", 1)
        .into_bytes())
}

fn source() -> Source {
    Source {
        kind: SourceKind::Gate,
        name: SELF_GATE.to_owned(),
        version: "test".to_owned(),
        role: None,
        run_key: "sha256:test".to_owned(),
    }
}

fn replay(entries: &[CorpusEntry], rubric: &[u8]) -> FindingsDocument {
    replay_golden(
        &GoldenReplay {
            corpus_dir: CORPUS_DIR,
            entries,
            rubric_path: RUBRIC_PATH,
            rubric,
        },
        source(),
    )
}

/// The rules of the document's findings, in order.
fn rules(document: &FindingsDocument) -> Vec<&str> {
    document
        .findings
        .iter()
        .map(|finding| finding.rule.as_deref().unwrap_or_default())
        .collect()
}

/// The case directories of `entries`.
fn case_names(entries: &[CorpusEntry]) -> Vec<&str> {
    entries
        .iter()
        .filter(|entry| entry.kind == CorpusEntryKind::Directory)
        .map(|entry| entry.path.as_str())
        .collect()
}

#[test]
fn the_recorded_corpus_replays_clean() -> TestResult {
    let document = replay(&recorded_corpus()?, &recorded_rubric()?);

    assert_eq!(
        (document.conclusion, rules(&document)),
        (Some(GateConclusion::Pass), Vec::new()),
        "{:#?}",
        document.findings
    );
    assert_eq!(document.source, source(), "the document names the gate");
    Ok(())
}

#[test]
fn a_rubric_change_without_a_snapshot_update_fails_every_case() -> TestResult {
    let entries = recorded_corpus()?;

    let document = replay(&entries, &changed_rubric()?);

    let files: Vec<String> = case_names(&entries)
        .iter()
        .map(|case| format!("{CORPUS_DIR}/{case}/decision.json"))
        .collect();
    assert_eq!(
        document.conclusion,
        Some(GateConclusion::Fail),
        "a byte difference fails the gate"
    );
    assert_eq!(
        document
            .findings
            .iter()
            .map(|finding| (finding.rule.as_deref(), finding.file.as_str()))
            .collect::<Vec<_>>(),
        files
            .iter()
            .map(|file| (Some("golden-mismatch"), file.as_str()))
            .collect::<Vec<_>>(),
        "the rubric is in every inputs digest, so every case differs"
    );
    Ok(())
}

#[test]
fn a_rubric_change_with_every_snapshot_updated_passes_the_replay() -> TestResult {
    let rubric_text = changed_rubric()?;
    let rubric: Rubric = toml::from_str(std::str::from_utf8(&rubric_text)?)?;
    let mut entries = recorded_corpus()?;
    let mut snapshots = Vec::new();
    for entry in &entries {
        let (CorpusEntryKind::File(inputs), Some(case)) =
            (&entry.kind, entry.path.strip_suffix("/inputs.json"))
        else {
            continue;
        };
        let inputs: DecisionInputs = serde_json::from_slice(inputs)?;
        let mut snapshot = serde_json::to_vec_pretty(&decide(&rubric, &inputs))?;
        snapshot.push(b'\n');
        snapshots.push((format!("{case}/decision.json"), snapshot));
    }
    for (decision_path, snapshot) in snapshots {
        let entry = entries
            .iter_mut()
            .find(|entry| entry.path == decision_path)
            .ok_or("every case has a decision.json")?;
        entry.kind = CorpusEntryKind::File(snapshot);
    }

    let document = replay(&entries, &rubric_text);

    assert_eq!(
        (document.conclusion, rules(&document)),
        (Some(GateConclusion::Pass), Vec::new()),
        "an updated snapshot matches; its approval is rule 4's, through the trust paths"
    );
    Ok(())
}

/// The recorded corpus with the file at `path` replaced by `kind`.
fn with_entry(path: &str, kind: CorpusEntryKind) -> Result<Vec<CorpusEntry>, Box<dyn Error>> {
    let mut entries = recorded_corpus()?;
    let entry = entries
        .iter_mut()
        .find(|entry| entry.path == path)
        .ok_or_else(|| format!("the corpus holds {path}"))?;
    entry.kind = kind;
    Ok(entries)
}

/// The recorded bytes of `path`.
fn recorded_file(path: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(repo_root().join(CORPUS_DIR).join(path))?)
}

/// The rule, file, line and evidence of each finding.
fn located(document: &FindingsDocument) -> Vec<(&str, &str, u64, &str)> {
    document
        .findings
        .iter()
        .map(|finding| {
            (
                finding.rule.as_deref().unwrap_or_default(),
                finding.file.as_str(),
                finding.line.get(),
                finding.evidence.as_str(),
            )
        })
        .collect()
}

#[test]
fn a_hand_edited_snapshot_fails_at_its_first_differing_line() -> TestResult {
    let path = "rule-9-success/decision.json";
    let edited = String::from_utf8(recorded_file(path)?)?.replacen(
        "\"conclusion\": \"success\"",
        "\"conclusion\": \"failure\"",
        1,
    );
    let entries = with_entry(path, CorpusEntryKind::File(edited.into_bytes()))?;

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        located(&document),
        vec![(
            "golden-mismatch",
            "corpus/golden/rule-9-success/decision.json",
            5,
            "\"conclusion\": \"failure\",",
        )],
        "the finding quotes the recorded line that differs"
    );
    Ok(())
}

#[test]
fn a_snapshot_missing_its_final_newline_fails_at_its_last_line() -> TestResult {
    let path = "rule-9-success/decision.json";
    let mut truncated = recorded_file(path)?;
    let lines = truncated.iter().filter(|byte| **byte == b'\n').count();
    truncated.pop();
    let entries = with_entry(path, CorpusEntryKind::File(truncated))?;

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        located(&document),
        vec![(
            "golden-mismatch",
            "corpus/golden/rule-9-success/decision.json",
            u64::try_from(lines)?,
            "}",
        )],
        "a difference past the recorded bytes is on their last line"
    );
    Ok(())
}

#[test]
fn unparseable_inputs_are_golden_inputs_invalid() -> TestResult {
    let path = "rule-9-success/inputs.json";
    let entries = with_entry(
        path,
        CorpusEntryKind::File(b"{\n  \"policy\": [\n".to_vec()),
    )?;

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        (document.conclusion, located(&document)),
        (
            Some(GateConclusion::Fail),
            vec![(
                "golden-inputs-invalid",
                "corpus/golden/rule-9-success/inputs.json",
                3,
                "corpus/golden/rule-9-success/inputs.json",
            )]
        ),
        "serde points past the last line, so the path is quoted"
    );
    Ok(())
}

#[test]
fn an_unparseable_rubric_is_the_only_finding() -> TestResult {
    let rubric = b"version = \"1\"\nmajors_block_threshold = 0\n";

    let document = replay(&recorded_corpus()?, rubric);

    assert_eq!(
        located(&document),
        vec![(
            "golden-rubric-invalid",
            RUBRIC_PATH,
            2,
            "majors_block_threshold = 0",
        )],
        "a zero threshold is not a NonZeroU32, and no case is replayed"
    );
    Ok(())
}

#[test]
fn a_case_without_a_snapshot_is_incomplete() -> TestResult {
    let mut entries = recorded_corpus()?;
    entries.retain(|entry| entry.path != "rule-9-success/decision.json");

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        rules_and_files(&document),
        vec![(
            "golden-case-incomplete",
            "corpus/golden/rule-9-success/decision.json"
        )],
        "the missing file is named"
    );
    Ok(())
}

#[test]
fn a_case_directory_without_inputs_is_incomplete() -> TestResult {
    let mut entries = recorded_corpus()?;
    entries.push(CorpusEntry {
        path: "zz-new-case".to_owned(),
        kind: CorpusEntryKind::Directory,
    });

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        rules_and_files(&document),
        vec![(
            "golden-case-incomplete",
            "corpus/golden/zz-new-case/inputs.json"
        )],
        "an empty case directory is not skipped"
    );
    Ok(())
}

/// The rule and file of each finding.
fn rules_and_files(document: &FindingsDocument) -> Vec<(&str, &str)> {
    document
        .findings
        .iter()
        .map(|finding| {
            (
                finding.rule.as_deref().unwrap_or_default(),
                finding.file.as_str(),
            )
        })
        .collect()
}

#[test]
fn every_entry_outside_the_allowlist_is_refused() -> TestResult {
    let mut entries = recorded_corpus()?;
    let file = || CorpusEntryKind::File(b"{}\n".to_vec());
    let extras = [
        ("notes.txt", file()),
        ("rule-9-success/expected.json", file()),
        ("rule-9-success/nested", CorpusEntryKind::Directory),
        ("rule-9-success/nested/inputs.json", file()),
        ("rule-9-success/link.json", CorpusEntryKind::Other),
        ("rule-9-success/inputs.json", file()),
        ("rule-9-success/../x", file()),
    ];
    for (path, kind) in extras {
        entries.push(CorpusEntry {
            path: path.to_owned(),
            kind,
        });
    }

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        rules_and_files(&document),
        vec![
            ("golden-unexpected-entry", "corpus/golden/notes.txt"),
            (
                "golden-unexpected-entry",
                "corpus/golden/rule-9-success/../x"
            ),
            (
                "golden-unexpected-entry",
                "corpus/golden/rule-9-success/expected.json"
            ),
            (
                "golden-unexpected-entry",
                "corpus/golden/rule-9-success/inputs.json"
            ),
            (
                "golden-unexpected-entry",
                "corpus/golden/rule-9-success/link.json"
            ),
            (
                "golden-unexpected-entry",
                "corpus/golden/rule-9-success/nested"
            ),
            (
                "golden-unexpected-entry",
                "corpus/golden/rule-9-success/nested/inputs.json"
            ),
        ],
        "a stray file, an extra or nested entry, a symlink, a repeated file and a `..` path"
    );
    Ok(())
}

#[test]
fn a_repeated_readme_or_case_directory_is_refused() -> TestResult {
    let mut entries = recorded_corpus()?;
    let repeats: Vec<CorpusEntry> = entries
        .iter()
        .filter(|entry| matches!(entry.path.as_str(), "README.md" | "rule-9-success"))
        .cloned()
        .collect();
    entries.extend(repeats);

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        rules_and_files(&document),
        vec![
            ("golden-unexpected-entry", "corpus/golden/README.md"),
            ("golden-unexpected-entry", "corpus/golden/rule-9-success"),
        ],
        "each entry is allowed once"
    );
    Ok(())
}

#[test]
fn an_empty_corpus_fails() -> TestResult {
    let entries = [CorpusEntry {
        path: "README.md".to_owned(),
        kind: CorpusEntryKind::File(b"# corpus\n".to_vec()),
    }];

    let document = replay(&entries, &recorded_rubric()?);

    assert_eq!(
        (document.conclusion, rules_and_files(&document)),
        (
            Some(GateConclusion::Fail),
            vec![("golden-corpus-empty", "corpus/golden/README.md")]
        ),
        "a replay of nothing checks nothing"
    );
    Ok(())
}

#[test]
fn passing_and_failing_documents_satisfy_the_findings_schema() -> TestResult {
    let validator = ContractValidator::new()?;
    let mut entries = recorded_corpus()?;
    entries.push(CorpusEntry {
        path: format!("x`\n[l](https://e){}", "y".repeat(200)),
        kind: CorpusEntryKind::Other,
    });
    let failing = replay(&entries, &changed_rubric()?);
    let passing = replay(&recorded_corpus()?, &recorded_rubric()?);

    for document in [&failing, &passing] {
        validator.read(Contract::Findings, &serde_json::to_vec(document)?)?;
    }
    let titles: Vec<(usize, bool)> = failing
        .findings
        .iter()
        .map(|finding| finding.title.as_str())
        .filter(|title| title.contains("corpus/golden/x"))
        .map(|title| {
            (
                title.chars().count(),
                title.starts_with(
                    "Unexpected entry in the golden corpus: `corpus/golden/x???l??https?e?yyy",
                ),
            )
        })
        .collect();
    assert_eq!(
        titles,
        vec![(100, true)],
        "a hostile name is reduced and clipped to the title cap"
    );
    Ok(())
}
