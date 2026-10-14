//! Z4's anchoring of gate findings at its bounds: long lines, many files, a byte budget, renames
//! and absurd symbols (review round 1 of #56).

use std::collections::BTreeSet;
use std::error::Error;

use assay_app::{
    ArtifactJob, AuthenticatedRun, ChangedFiles, MAX_EVIDENCE_BYTES, MAX_EVIDENCE_FILES,
    MAX_SYMBOL_BYTES,
};
use assay_domain::{FileChange, Fingerprint, GateConclusion, Lines};
use serde_json::{Value, json};

use crate::decide_fakes::{Scenario, authenticated, gate_document, gate_finding};
use crate::plan_fakes::{BASE, FakePr, HEAD};

/// The statement every fixture cites.
const STATEMENT: &str = "let x = y.unwrap();";

/// A function `f` whose second line is `line`.
fn in_function(line: &str) -> String {
    format!("fn f() {{\n{line}\n}}\n")
}

/// `findings` published as the failing `gates-lint` temper document of a scenario over `pr`.
fn lint_findings(pr: FakePr, findings: &[Value]) -> Result<AuthenticatedRun, Box<dyn Error>> {
    let mut scenario = Scenario::of(pr)?;
    scenario.publish_gate(
        (ArtifactJob::GatesLint, "temper"),
        GateConclusion::Fail,
        gate_document("temper", "fail", &json!(findings)),
    )?;
    authenticated(scenario.authenticate()?)
}

/// The rule each lint finding kept: `None` where Z4 could not verify it.
fn lint_rules(run: &AuthenticatedRun) -> Result<Vec<Option<&str>>, Box<dyn Error>> {
    let lint = run.gates().get(1).ok_or("the lint gate")?;
    Ok(lint.findings.iter().map(|f| f.rule.as_deref()).collect())
}

#[test]
fn many_findings_on_one_long_line_are_all_anchored_to_it() -> Result<(), Box<dyn Error>> {
    // Two MiB of blanks inside the statement normalise away, so every quote of it verifies;
    // each finding must cost its quote, not the line (H-13).
    let line = format!("    let x = {}y.unwrap();", " ".repeat(2 << 20));
    let mut pr = FakePr::new();
    pr.write(HEAD, "src/lib.rs", in_function(&line).as_bytes());
    let findings = vec![gate_finding("clippy::unwrap_used", "src/lib.rs", 2, STATEMENT); 5_000];

    let run = lint_findings(pr, &findings)?;

    let gate = run.gates().get(1).ok_or("the lint gate")?;
    let rules: BTreeSet<Option<&str>> = gate.findings.iter().map(|f| f.rule.as_deref()).collect();
    let fingerprints: BTreeSet<Fingerprint> = gate.findings.iter().map(|f| f.fp).collect();
    assert_eq!(
        (gate.findings.len(), rules, fingerprints.len()),
        (5_000, BTreeSet::from([Some("clippy::unwrap_used")]), 1)
    );
    Ok(())
}

#[test]
fn a_file_past_the_file_cap_leaves_its_findings_unverified() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    let mut findings = Vec::new();
    for index in 0..=MAX_EVIDENCE_FILES {
        let path = format!("cited/{index:05}.rs");
        pr.write(HEAD, &path, in_function(STATEMENT).as_bytes());
        findings.push(gate_finding("raised", &path, 2, STATEMENT));
    }

    let run = lint_findings(pr, &findings)?;

    let rules = lint_rules(&run)?;
    let (last, read) = rules.split_last().ok_or("findings")?;
    assert_eq!(
        (read.iter().all(|rule| *rule == Some("raised")), *last),
        (true, None)
    );
    Ok(())
}

#[test]
fn files_past_the_byte_budget_leave_their_findings_unverified() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    let text = in_function(STATEMENT);
    pr.write_listed_as((HEAD, "a.rs"), text.as_bytes(), MAX_EVIDENCE_BYTES - 10);
    pr.write_listed_as((HEAD, "b.rs"), text.as_bytes(), 20);
    pr.write_listed_as((HEAD, "c.rs"), text.as_bytes(), 1);
    let findings = ["a.rs", "b.rs", "c.rs"].map(|file| gate_finding("raised", file, 2, STATEMENT));

    let run = lint_findings(pr, &findings)?;

    // `b.rs` does not fit what is left, and the reads stop there: `c.rs` is not read either.
    assert_eq!(lint_rules(&run)?, vec![Some("raised"), None, None]);
    Ok(())
}

#[test]
fn a_base_side_finding_on_a_renamed_file_is_fingerprinted_at_its_head_path()
-> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    let text = in_function(&format!("    {STATEMENT}"));
    pr.write(BASE, "old.rs", text.as_bytes());
    pr.write(HEAD, "new.rs", text.as_bytes());
    pr.changes = ChangedFiles::Listed(vec![FileChange {
        renamed_from: Some("old.rs".to_owned()),
        ..FileChange::new(
            "new.rs",
            Lines::Counted {
                added: 0,
                removed: 0,
            },
        )
    }]);
    let mut on_base = gate_finding("raised", "old.rs", 2, STATEMENT);
    on_base["side"] = json!("LEFT");
    let on_head = gate_finding("raised", "new.rs", 2, STATEMENT);

    let run = lint_findings(pr, &[on_base, on_head])?;

    let lint = run.gates().get(1).ok_or("the lint gate")?;
    let anchored: Vec<(Option<&str>, Fingerprint)> = lint
        .findings
        .iter()
        .map(|f| (f.rule.as_deref(), f.fp))
        .collect();
    let head_fp = anchored.get(1).ok_or("the head finding")?.1;
    assert_eq!(
        anchored,
        vec![(Some("raised"), head_fp), (Some("raised"), head_fp)],
        "both quote one line of one file, read on each side"
    );
    Ok(())
}

#[test]
fn a_function_name_past_the_symbol_cap_leaves_its_file_unverified() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    let name = "f".repeat(MAX_SYMBOL_BYTES + 1);
    pr.write(
        HEAD,
        "src/lib.rs",
        format!("fn {name}() {{\n    {STATEMENT}\n}}\n").as_bytes(),
    );
    let findings = [gate_finding("raised", "src/lib.rs", 2, STATEMENT)];

    let run = lint_findings(pr, &findings)?;

    assert_eq!(lint_rules(&run)?, vec![None]);
    Ok(())
}
