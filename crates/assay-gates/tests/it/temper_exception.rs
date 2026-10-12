//! Step 2, `exception`: every `ask` hit of the pinned `temper scan --all --json` must be named in
//! base `.assay/exceptions.toml` (`docs/architecture/03-contracts.md` §1, §2.1).

use std::assert_matches;
use std::error::Error;

use assay_app::{Conclusion, GateJob, GateOutcome, Termination};
use assay_domain::Severity;
use assay_gates::{Revision, TemperError};

use crate::temper_support::{EXCEPTIONS, Pr, hit};

const FAILED: GateOutcome = GateOutcome::Concluded(Conclusion::Fail);
const PASSED: GateOutcome = GateOutcome::Concluded(Conclusion::Pass);
const PROCESS: &str = "crates/assay-forge/src/process.rs";
const EXPECT: &str = "expect-attr";
const LINT: &str = "clippy::too_many_lines";

/// An exceptions file admitting [`LINT`] in [`PROCESS`].
const ADMITS_PROCESS: &str = r#"
[[exception]]
path = "crates/assay-forge/src/process.rs"
lint = "clippy::too_many_lines"
reason = "one state machine, split across helpers it would only obscure"
"#;

/// A Rust source with `attribute` starting on line `line`.
fn source(line: usize, attribute: &str) -> String {
    format!(
        "{}{attribute}\nfn run() {{}}\n",
        "// filler\n".repeat(line - 1)
    )
}

/// A PR whose head [`PROCESS`] holds `attribute` at line 42, which the scan reports as an
/// `#[expect]` of [`LINT`].
fn pr_with(attribute: &str) -> Result<Pr, Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, PROCESS, &source(42, attribute));
    pr.scan_prints(&[hit(EXPECT, (PROCESS, 42), "ask", Some(LINT))]);
    Ok(pr)
}

#[test]
fn an_exception_not_named_in_base_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with("#[expect(clippy::too_many_lines, reason = \"one state machine\")]")?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "exception".to_owned(),
                PROCESS.to_owned(),
                42,
                Severity::Blocker
            )]
        ),
        "with no exceptions file nothing is admitted"
    );
    Ok(())
}

#[test]
fn an_exception_named_in_base_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with("#[expect(clippy::too_many_lines, reason = \"one state machine\")]")?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "the base admits this one");
    Ok(())
}

#[test]
fn a_wrapped_single_lint_expect_is_read_from_the_source() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with(
        "#[expect(\n    clippy::too_many_lines,\n    reason = r#\"one \"state\" machine, (split)\"#\n)]",
    )?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "rustfmt wraps long attributes; the whole attribute is parsed"
    );
    Ok(())
}

#[test]
fn a_multi_lint_expect_is_not_admitted_by_its_first_lint() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with(
        "#[expect(clippy::too_many_lines, clippy::unwrap_used, reason = \"one state machine\")]",
    )?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "exception".to_owned(),
                PROCESS.to_owned(),
                42,
                Severity::Blocker
            )]
        ),
        "temper names only the first lint; the second would ride along"
    );
    Ok(())
}

#[test]
fn a_second_expect_on_the_reported_line_must_be_admitted_too() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with(
        "#[expect(clippy::too_many_lines, reason = \"a\")] #[expect(clippy::unwrap_used, reason = \"b\")]",
    )?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "exception".to_owned(),
                PROCESS.to_owned(),
                42,
                Severity::Blocker
            )]
        ),
        "the line's other attribute is not admitted"
    );
    Ok(())
}

#[test]
fn an_expect_the_scan_did_not_report_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        PROCESS,
        &source(7, "#[expect(clippy::too_many_lines, reason = \"r\")]"),
    );
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "exception".to_owned(),
                PROCESS.to_owned(),
                7,
                Severity::Blocker
            )]
        ),
        "an exception is admitted only through a hit the scan reported"
    );
    Ok(())
}

#[test]
fn an_expect_hit_whose_source_cannot_be_read_is_not_admitted() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with("fn run() {}")?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);

    let outcome = pr.outcome()?;

    assert_eq!(outcome.0, FAILED, "no attribute at the reported line");
    Ok(())
}

#[test]
fn a_covered_hit_is_admitted_by_file_and_lint() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);
    pr.scan_prints(&[hit("panic-macro", (PROCESS, 50), "ask", Some(LINT))]);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "the attribute that covers it is checked through its own expect-attr hit"
    );
    Ok(())
}

#[test]
fn an_exceptions_file_only_at_head_admits_nothing() -> Result<(), Box<dyn Error>> {
    let mut pr = pr_with("#[expect(clippy::too_many_lines, reason = \"one state machine\")]")?;
    pr.set(Revision::Head, EXCEPTIONS, ADMITS_PROCESS);

    let outcome = pr.outcome()?;

    assert_eq!(outcome.0, FAILED, "a PR cannot admit its own exception");
    Ok(())
}

#[test]
fn the_same_lint_in_another_file_is_not_admitted() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);
    pr.scan_prints(&[hit("panic-macro", ("src/lib.rs", 7), "ask", Some(LINT))]);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "exception".to_owned(),
                "src/lib.rs".to_owned(),
                7,
                Severity::Blocker
            )]
        ),
        "an exception is admitted for its file only"
    );
    Ok(())
}

#[test]
fn an_ask_hit_without_a_lint_is_never_admitted() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, EXCEPTIONS, ADMITS_PROCESS);
    pr.scan_prints(&[hit("panic-macro", (PROCESS, 42), "ask", None)]);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome.0, FAILED,
        "an exception request naming no lint fails"
    );
    Ok(())
}

#[test]
fn hard_and_soft_hits_are_left_to_the_other_jobs() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.scan.termination = Termination::Exited(1);
    pr.scan_prints(&[
        hit("panic-macro", (PROCESS, 1), "hard", None),
        hit("commented-code", (PROCESS, 2), "soft", None),
    ]);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "exit 1 means hard hits, which baseline --check counts in gates-lint"
    );
    Ok(())
}

#[test]
fn a_scan_that_does_not_exit_0_or_1_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    for termination in [
        Termination::Exited(2),
        Termination::Signalled,
        Termination::TimedOut,
        Termination::OutputTooLarge,
    ] {
        let mut pr = Pr::new()?;
        pr.scan.termination = termination;

        let result = pr.run(GateJob::Static)?;

        assert_matches!(
            result,
            Err(TemperError::ScanEnded { termination: ended }) if ended == termination
        );
    }
    Ok(())
}

#[test]
fn unparsable_scan_output_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    for stdout in [
        "temper: not a Rust project\n",
        r#"{"hits": []}"#,
        r#"[{"file": "a.rs", "line": 0, "severity": "ask", "exception": "x"}]"#,
        r#"[{"file": "a.rs", "line": 3, "severity": "fatal"}]"#,
        r#"[{"line": 3, "severity": "ask", "exception": "x"}]"#,
    ] {
        let mut pr = Pr::new()?;
        pr.scan.stdout = stdout.as_bytes().to_vec();

        let result = pr.run(GateJob::Static)?;

        assert_matches!(result, Err(TemperError::ScanOutput { .. }), "{stdout}");
    }
    Ok(())
}

#[test]
fn an_unparsable_exceptions_file_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    for text in [
        "[[exception]\n",
        "[[exception]]\npath = \"a.rs\"\nlint = \"x\"\n",
        "[[exception]]\npath = \"a.rs\"\nlint = \"x\"\nreason = \"\"\n",
        "[[exception]]\npath = \"a.rs\"\nlint = \"x\"\nreason = \"r\"\nline = 3\n",
        "[[allow]]\nlint = \"x\"\n",
    ] {
        let mut pr = Pr::new()?;
        pr.set(Revision::Base, EXCEPTIONS, text);

        let result = pr.run(GateJob::Static)?;

        assert_matches!(
            result,
            Err(TemperError::Malformed { revision: Revision::Base, path, .. }) if path == EXCEPTIONS,
            "{text}"
        );
    }
    Ok(())
}
