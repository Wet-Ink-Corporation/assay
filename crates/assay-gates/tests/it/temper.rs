//! The temper adapter in `gates-static` (Z2s, `docs/architecture/03-contracts.md` §2.1 steps
//! 1-3): its settings, where it runs, how it runs the pinned scan, how it reads the PR, and
//! step 1, `raised`.

use std::assert_matches;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use assay_app::{BuildJob, Conclusion, GateError, GateJob, GateOutcome, GateRunner};
use assay_domain::Severity;
use assay_gates::{
    EntryKind, GateEnvironment, Revision, RevisionFiles, TemperAdapter, TemperError, TemperPaths,
    TemperSettings, TemperSettingsError, TreeEntry,
};

use crate::support::{config, plugin};
use crate::temper_support::{BASELINE, CRATES, FakeError, NODE, Pr, baseline, baseline_with};

const FAILED: GateOutcome = GateOutcome::Concluded(Conclusion::Fail);
const PASSED: GateOutcome = GateOutcome::Concluded(Conclusion::Pass);
const NOT_RUN: GateOutcome = GateOutcome::Error(GateError::NotRun);

fn path_only(path: &str) -> GateEnvironment {
    GateEnvironment::from_vars([(OsString::from("PATH"), OsString::from(path))])
}

#[test]
fn settings_refuse_a_relative_node() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let paths = TemperPaths {
        node: PathBuf::from("node"),
        ..pr.paths()
    };

    let settings = TemperSettings::new(paths, &path_only("/usr/bin"), "run-1");

    assert!(
        matches!(&settings, Err(TemperSettingsError::RelativeNode { path }) if path.as_os_str() == "node"),
        "node is never looked up on a PATH, got {settings:?}"
    );
    Ok(())
}

#[test]
fn settings_refuse_a_relative_pinned_copy() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let paths = TemperPaths {
        pinned: PathBuf::from("pinned"),
        ..pr.paths()
    };

    let settings = TemperSettings::new(paths, &path_only("/usr/bin"), "run-1");

    assert!(
        matches!(&settings, Err(TemperSettingsError::RelativePinned { path }) if path.as_os_str() == "pinned"),
        "got {settings:?}"
    );
    Ok(())
}

#[test]
fn settings_refuse_a_pinned_copy_that_is_not_utf8() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let paths = TemperPaths {
        pinned: PathBuf::from(OsStr::from_bytes(b"/opt/temper-\xff")),
        ..pr.paths()
    };

    let settings = TemperSettings::new(paths, &path_only("/usr/bin"), "run-1");

    assert!(
        matches!(settings, Err(TemperSettingsError::NonUtf8Pinned { .. })),
        "node takes the script as text, got {settings:?}"
    );
    Ok(())
}

#[test]
fn settings_refuse_a_relative_or_root_head_tree() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    for head_tree in ["head", "/"] {
        let paths = TemperPaths {
            head_tree: PathBuf::from(head_tree),
            ..pr.paths()
        };

        let settings = TemperSettings::new(paths, &path_only("/usr/bin"), "run-1");

        assert!(
            matches!(&settings, Err(TemperSettingsError::HeadTree { path }) if path.as_os_str() == head_tree),
            "{head_tree}: got {settings:?}"
        );
    }
    Ok(())
}

#[test]
fn settings_refuse_an_empty_run_key() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;

    let settings = TemperSettings::new(pr.paths(), &path_only("/usr/bin"), "");

    assert!(
        matches!(settings, Err(TemperSettingsError::EmptyRunKey)),
        "the document requires a run key, got {settings:?}"
    );
    Ok(())
}

#[test]
fn settings_refuse_a_path_with_a_relative_entry() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    for path in ["/usr/bin:bin", "/usr/bin::/bin", ".:/usr/bin", "/usr/bin:"] {
        let settings = TemperSettings::new(pr.paths(), &path_only(path), "run-1");

        assert!(
            matches!(settings, Err(TemperSettingsError::RelativeSearchPath)),
            "{path}: temper's own git would be found from the head tree, got {settings:?}"
        );
    }
    Ok(())
}

#[test]
fn refuses_gates_test_before_running_anything() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;

    let result = pr.run(GateJob::Build(BuildJob::Test))?;

    assert_matches!(
        result,
        Err(TemperError::UnsupportedJob {
            job: GateJob::Build(BuildJob::Test)
        })
    );
    assert_eq!(pr.scan.seen, Vec::new(), "nothing ran");
    Ok(())
}

#[test]
fn refuses_a_plugin_that_is_not_temper() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let config = config()?;
    let custom = plugin(&config, "custom-lint")?;
    let settings = pr.settings()?;
    let mut adapter = TemperAdapter::new(&mut pr.scan, &pr.revisions, settings);

    let result = adapter.run(&custom, GateJob::Static);

    assert_matches!(result, Err(TemperError::NotTemper { plugin }) if plugin == "custom-lint");
    Ok(())
}

#[test]
fn an_unchanged_pr_passes_with_a_valid_document_and_no_findings() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "nothing changed");
    Ok(())
}

#[test]
fn runs_only_the_pinned_scan_with_the_configured_node() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;

    pr.outcome()?;

    let [seen] = pr.scan.seen.as_slice() else {
        return Err(format!("expected one scan, got {:?}", pr.scan.seen).into());
    };
    let script = pr.pinned.join("temper.mjs");
    let ceiling = pr.head_tree.parent().ok_or("the head tree has a parent")?;
    let timeout = plugin(&config()?, "temper")?.timeout();
    assert_eq!(
        (
            seen.program.as_os_str(),
            seen.args.clone(),
            seen.cwd.as_path(),
            seen.timeout
        ),
        (
            OsStr::new(NODE),
            vec![
                script.to_string_lossy().into_owned(),
                "scan".to_owned(),
                "--all".to_owned(),
                "--json".to_owned()
            ],
            pr.head_tree.as_path(),
            timeout
        ),
        "the configured node runs the base's pinned temper.mjs over the head tree"
    );
    assert_eq!(
        seen.env,
        [
            ("PATH", "/usr/bin:/bin"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .into_iter()
        .chain([(
            OsString::from("GIT_CEILING_DIRECTORIES"),
            ceiling.as_os_str().to_owned()
        )])
        .collect::<Vec<_>>(),
        "the scan sees the allowlist and git hardening, and git cannot find a repository above the tree"
    );
    Ok(())
}

#[test]
fn a_reader_that_fails_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.revisions.failing = Some(BASELINE.to_owned());

    let result = pr.run(GateJob::Static)?;

    assert_matches!(
        result,
        Err(TemperError::Revision { revision: Revision::Base, path, source: FakeError }) if path == BASELINE
    );
    Ok(())
}

#[test]
fn a_directory_where_a_file_belongs_is_not_a_file() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.remove(Revision::Head, BASELINE);
    pr.set(Revision::Head, &format!("{BASELINE}/a"), "{}");
    pr.set(Revision::Head, &format!("{BASELINE}/b"), "{}");

    let result = pr.run(GateJob::Static)?;

    assert_matches!(
        result,
        Err(TemperError::NotAFile { revision: Revision::Head, path }) if path == BASELINE
    );
    Ok(())
}

/// A reader that answers every listing with an entry at another path.
#[derive(Debug)]
struct Elsewhere;

impl RevisionFiles for Elsewhere {
    type Error = FakeError;

    fn list(&self, _: Revision, _: Option<&str>) -> Result<Vec<TreeEntry>, FakeError> {
        Ok(vec![TreeEntry {
            path: "elsewhere.json".to_owned(),
            kind: EntryKind::File,
            object: "0".repeat(40),
            size: Some(2),
        }])
    }

    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, FakeError> {
        Ok(entries.iter().map(|_| b"{}".to_vec()).collect())
    }

    fn blob_id(&self, _: &[u8]) -> Result<String, FakeError> {
        Ok("0".repeat(40))
    }
}

#[test]
fn a_listing_of_another_path_is_not_the_file() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let temper = plugin(&config()?, "temper")?;
    let settings = pr.settings()?;
    let mut adapter = TemperAdapter::new(&mut pr.scan, Elsewhere, settings);

    let result = adapter.run(&temper, GateJob::Static);

    assert_matches!(
        result,
        Err(TemperError::NotAFile {
            revision: Revision::Base,
            ..
        })
    );
    Ok(())
}

#[test]
fn a_raised_count_fails_at_its_line_in_the_head_baseline() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let raised = baseline(&[("assay-forge", "clippy::too_many_lines", 3)]);
    pr.set(Revision::Head, BASELINE, &raised);

    let outcome = pr.outcome()?;

    let line = line_of(&raised, "clippy::too_many_lines")?;
    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "raised".to_owned(),
                BASELINE.to_owned(),
                line,
                Severity::Blocker
            )]
        ),
        "2 -> 3 is a raise"
    );
    Ok(())
}

#[test]
fn a_new_baseline_entry_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let added = baseline(&[
        ("assay-forge", "clippy::too_many_lines", 2),
        ("assay-gates", "clippy::missing_errors_doc", 1),
    ]);
    pr.set(Revision::Head, BASELINE, &added);

    let outcome = pr.outcome()?;

    let line = line_of(&added, "clippy::missing_errors_doc")?;
    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "raised".to_owned(),
                BASELINE.to_owned(),
                line,
                Severity::Blocker
            )]
        ),
        "recording new debt is a raise"
    );
    Ok(())
}

#[test]
fn a_pure_decrease_in_the_baseline_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.both(
        BASELINE,
        &baseline(&[
            ("assay-forge", "clippy::too_many_lines", 2),
            ("assay-gates", "clippy::missing_errors_doc", 4),
        ]),
    );
    pr.set(
        Revision::Head,
        BASELINE,
        &baseline(&[("assay-gates", "clippy::missing_errors_doc", 1)]),
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "one count lowered, one entry paid off"
    );
    Ok(())
}

#[test]
fn a_crate_added_or_moved_in_the_baseline_fails() -> Result<(), Box<dyn Error>> {
    let counts = [("assay-forge", "clippy::too_many_lines", 2)];
    for crates in [
        vec![CRATES[0], CRATES[1], ("assay-new", "crates/assay-new")],
        vec![CRATES[0], ("assay-gates", "crates/elsewhere")],
    ] {
        let mut pr = Pr::new()?;
        let head = baseline_with(&counts, &crates);
        pr.set(Revision::Head, BASELINE, &head);

        let outcome = pr.outcome()?;

        let name = crates.last().map_or("", |(name, _)| *name);
        assert_eq!(
            outcome,
            (
                FAILED,
                vec![(
                    "raised".to_owned(),
                    BASELINE.to_owned(),
                    line_of(&head, name)?,
                    Severity::Blocker
                )]
            ),
            "`crates` decides which directory a count covers: {crates:?}"
        );
    }
    Ok(())
}

#[test]
fn a_crate_removed_from_the_baseline_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let counts = [("assay-forge", "clippy::too_many_lines", 2)];
    pr.set(
        Revision::Head,
        BASELINE,
        &baseline_with(&counts, &[CRATES[0]]),
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "a pure removal allows nothing new"
    );
    Ok(())
}

#[test]
fn a_crate_name_never_reaches_a_title_as_a_link() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let evil = "https://evil.example/x";
    pr.set(
        Revision::Head,
        BASELINE,
        &baseline(&[
            ("assay-forge", "clippy::too_many_lines", 2),
            (evil, "clippy::x", 1),
        ]),
    );

    let (outcome, document) = pr.document()?;

    let titles: Vec<String> = document
        .map(|document| document.findings.into_iter().map(|f| f.title).collect())
        .unwrap_or_default();
    assert_eq!(outcome, FAILED, "a new entry");
    assert!(
        titles.iter().all(|title| !title.contains("://")),
        "a URL in a crate name must not render as a link: {titles:?}"
    );
    Ok(())
}

#[test]
fn a_baseline_missing_on_either_side_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    for side in [Revision::Base, Revision::Head] {
        let mut pr = Pr::new()?;
        pr.remove(side, BASELINE);

        let result = pr.run(GateJob::Static)?;

        assert_matches!(
            result,
            Err(TemperError::Missing { revision, path }) if revision == side && path == BASELINE,
            "{side}"
        );
        assert_eq!(pr.outcome()?.0, NOT_RUN, "never a pass ({side})");
    }
    Ok(())
}

#[test]
fn an_unparsable_baseline_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    for text in [
        "{ not json",
        r#"{"temper_baseline": 2, "totals": {}}"#,
        r#"{"temper_baseline": 1}"#,
        r#"{"temper_baseline": 1, "totals": {"c": {"lint": -1}}}"#,
        r#"{"temper_baseline": 1, "totals": {"c": {"lint": 1.5}}}"#,
        r#"{"temper_baseline": 1, "totals": {}, "crates": {"c": 3}}"#,
    ] {
        let mut pr = Pr::new()?;
        pr.set(Revision::Head, BASELINE, text);

        let result = pr.run(GateJob::Static)?;

        assert_matches!(
            result,
            Err(TemperError::Malformed { revision: Revision::Head, path, .. }) if path == BASELINE,
            "{text}"
        );
    }
    Ok(())
}

#[test]
fn a_baseline_that_is_not_a_regular_file_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.link(Revision::Base, BASELINE, "elsewhere.json");

    let result = pr.run(GateJob::Static)?;

    assert_matches!(
        result,
        Err(TemperError::NotAFile { revision: Revision::Base, path }) if path == BASELINE
    );
    Ok(())
}

/// The 1-based line of the first line of `text` that contains `needle` in quotes.
fn line_of(text: &str, needle: &str) -> Result<u64, Box<dyn Error>> {
    let quoted = format!("\"{needle}\"");
    let index = text
        .lines()
        .position(|line| line.contains(&quoted))
        .ok_or_else(|| format!("{needle} is not in the text"))?;
    Ok(u64::try_from(index)? + 1)
}
