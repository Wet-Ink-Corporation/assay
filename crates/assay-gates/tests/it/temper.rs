//! The temper adapter in `gates-static` (Z2s, `docs/architecture/03-contracts.md` §2.1 steps
//! 1-3): where it runs, how it runs the pinned scan, and step 1, `raised`.

use std::assert_matches;
use std::error::Error;
use std::ffi::OsString;

use assay_app::{BuildJob, Conclusion, GateError, GateJob, GateOutcome, GateRunner};
use assay_domain::Severity;
use assay_gates::{Revision, TemperError};

use crate::support::{config, plugin};
use crate::temper_support::{BASELINE, Pr, baseline};

const FAILED: GateOutcome = GateOutcome::Concluded(Conclusion::Fail);
const PASSED: GateOutcome = GateOutcome::Concluded(Conclusion::Pass);
const NOT_RUN: GateOutcome = GateOutcome::Error(GateError::NotRun);

#[test]
fn refuses_every_job_but_gates_static_before_running_anything() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;

    let result = pr.run(GateJob::Build(BuildJob::Lint))?;

    assert_matches!(
        result,
        Err(TemperError::OutsideStatic {
            job: GateJob::Build(BuildJob::Lint)
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
    let mut adapter = assay_gates::TemperAdapter::new(&mut pr.scan, &pr.revisions, settings);

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
fn runs_only_the_pinned_scan_over_the_head_tree() -> Result<(), Box<dyn Error>> {
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
            OsString::from("node").as_os_str(),
            vec![
                script.to_string_lossy().into_owned(),
                "scan".to_owned(),
                "--all".to_owned(),
                "--json".to_owned()
            ],
            pr.head_tree.as_path(),
            timeout
        ),
        "the base's pinned temper.mjs scans the head tree under the plugin's timeout"
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
    pr.special(Revision::Base, BASELINE);

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
