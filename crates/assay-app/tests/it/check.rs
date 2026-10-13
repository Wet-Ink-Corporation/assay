//! `check_local`: `assay check --local` over a dirty working tree (02 §6, R-10, P9), driven
//! through fakes of every port (TST-09).

use std::assert_matches;
use std::error::Error;

use assay_app::CheckPorts;
use assay_app::{
    BaseConfigError, ChangedFiles, CheckError, Contract, ContractValidator, GateJob,
    MAX_DIFF_BYTES, MAX_FILE_LIST_BYTES, ModelOutcome, NoEvents, PlanError, ReadContractError,
    ReviewStatus, Structured, check_local,
};
use assay_domain::{Conclusion, FileChange, Lines, ReasonCode, Severity, Tier};
use serde_json::{Value, json};

use crate::check_fakes::{
    FakeGates, FakeModel, FakePatches, NoCli, Setup, TREE, Unreadable, dirty_tree, unwrap_finding,
};
use crate::plan_fakes::{BASE, FakePr, HEAD};

pub(crate) fn validator() -> Result<ContractValidator, Box<dyn Error>> {
    Ok(ContractValidator::new()?)
}

/// Checks `pr` with the `unwrap` patch, `gates` and `model`.
pub(crate) fn check(
    pr: &FakePr,
    gates: &mut FakeGates,
    model: &FakeModel,
) -> Result<assay_app::LocalReport, Box<dyn Error>> {
    let setup = Setup::of(pr)?;
    let patches = FakePatches::lib();
    let ports = CheckPorts {
        objects: pr,
        patches: &patches,
        gates,
        model,
        events: &mut NoEvents,
    };
    Ok(check_local(&setup.request(), ports, &validator()?)?)
}

#[test]
fn a_dirty_tree_is_planned_gated_reviewed_and_decided() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::finding(&[unwrap_finding()]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.tree(), TREE);
    assert_eq!(
        report
            .gates()
            .iter()
            .map(|gate| (gate.name.as_str(), gate.job, gate.outcome.as_str()))
            .collect::<Vec<_>>(),
        [("unit", "gates-test", "pass")]
    );
    assert_eq!(report.review().status, ReviewStatus::Completed);
    assert_eq!(
        report
            .review()
            .findings
            .iter()
            .map(|finding| (finding.finding.title.as_str(), finding.finding.line.get()))
            .collect::<Vec<_>>(),
        [("unwrap on user input panics", 2)]
    );
    assert_eq!(report.decision().conclusion, Conclusion::Success);
    assert_eq!(report.decision().counts.major, 1, "the finding counts");
    Ok(())
}

#[test]
fn the_model_is_called_once_with_the_role_the_pack_and_the_findings_schema()
-> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::finding(&[]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    let calls = model.calls.borrow();
    let [call] = calls.as_slice() else {
        return Err(format!("expected one call, got {}", calls.len()).into());
    };
    let role = report
        .review()
        .role
        .ok_or("a reviewed tier names its role")?;
    assert_eq!(report.tier(), Tier::Micro, "a one-line edit is micro");
    assert_eq!(role.code(), "combined-micro");
    assert_eq!(
        call.system_prompt_file.to_str(),
        Some("/bundle/v1/system.md")
    );
    assert_eq!(call.add_dir.to_str(), Some("/scratch/tree"));
    assert_eq!(
        (call.max_turns, call.budget),
        (role.max_turns(), role.cap())
    );
    let schema: Value = serde_json::from_str(&call.json_schema)?;
    assert_eq!(schema["title"], "assay.findings/1");
    let prompt = call.user_prompt.as_str();
    assert!(
        prompt.starts_with("Changed files (1):\nM src/lib.rs\n\nDiff:\ndiff --git a/src/lib.rs"),
        "the pack lists the files, then the diff: {prompt:?}"
    );
    assert!(
        prompt.ends_with("\n\nINSTRUCTION combined-micro"),
        "the role's instruction follows the pack: {prompt:?}"
    );
    Ok(())
}

#[test]
fn a_change_without_an_llm_stage_calls_no_model() -> Result<(), Box<dyn Error>> {
    let mut pr = dirty_tree();
    pr.write(HEAD, "README.md", b"# changed\n");
    pr.write(
        HEAD,
        "src/lib.rs",
        crate::check_fakes::LIB_AT_BASE.as_bytes(),
    );
    pr.write(
        BASE,
        "src/lib.rs",
        crate::check_fakes::LIB_AT_BASE.as_bytes(),
    );
    pr.changes = ChangedFiles::Listed(vec![FileChange::new(
        "README.md",
        Lines::Counted {
            added: 1,
            removed: 1,
        },
    )]);
    let model = FakeModel::finding(&[]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.tier(), Tier::Trivial);
    assert_eq!(model.call_count(), 0, "a trivial change gets no review");
    assert_eq!(report.review().status, ReviewStatus::NotRun);
    assert_eq!(report.decision().conclusion, Conclusion::Success);
    Ok(())
}

#[test]
fn a_base_without_config_is_refused_by_plan() -> Result<(), Box<dyn Error>> {
    let mut pr = dirty_tree();
    pr.remove(BASE, ".assay/plugins.toml");
    let setup = Setup::of(&pr)?;
    let model = FakeModel::default();
    let ports = CheckPorts {
        objects: &pr,
        patches: &FakePatches::lib(),
        gates: &mut FakeGates::passing(),
        model: &model,
        events: &mut NoEvents,
    };

    let result = check_local(&setup.request(), ports, &validator()?);

    assert_matches!(
        result,
        Err(CheckError::Plan(PlanError::BaseConfig(
            BaseConfigError::Missing
        )))
    );
    assert_eq!(model.call_count(), 0);
    Ok(())
}

#[test]
fn a_required_gate_that_does_not_run_locally_fails_the_decision() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::finding(&[]);

    let report = check(&pr, &mut FakeGates::default(), &model)?;

    let [gate] = report.gates() else {
        return Err("one gate is scheduled".into());
    };
    assert_eq!(
        (gate.outcome.as_str(), gate.cause.as_deref()),
        ("gate-error:not-run", Some("not run in local mode"))
    );
    assert_eq!(report.decision().conclusion, Conclusion::Failure);
    assert_eq!(
        report
            .decision()
            .reasons
            .iter()
            .map(|reason| reason.code)
            .collect::<Vec<_>>(),
        [ReasonCode::GateError]
    );
    Ok(())
}

#[test]
fn a_failing_gate_reports_its_findings() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let finding = json!({
        "class": "error-handling", "rule": "H-02", "kind": "defect", "severity": "blocker",
        "file": "src/lib.rs", "line": 2, "evidence": "input.parse().unwrap()",
        "title": "used `unwrap()`", "body": "H-02: no unwrap in production code.",
    });
    let mut gates = FakeGates::concluding("unit", "fail", &json!([finding]));

    let report = check(&pr, &mut gates, &FakeModel::finding(&[]))?;

    let [gate] = report.gates() else {
        return Err("one gate is scheduled".into());
    };
    assert_eq!(gate.outcome, "fail");
    assert_eq!(
        gate.findings
            .iter()
            .map(|finding| (
                finding.finding.rule.as_deref(),
                finding.finding.evidence.as_str()
            ))
            .collect::<Vec<_>>(),
        [(Some("H-02"), "    input.parse().unwrap()")],
        "a verified gate finding keeps its rule and quotes the tree's line"
    );
    assert_eq!(report.decision().conclusion, Conclusion::Failure);
    Ok(())
}

#[test]
fn a_gate_finding_without_content_to_anchor_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let finding = json!({
        "class": "error-handling", "kind": "defect", "severity": "minor",
        "file": "src/lib.rs", "line": 2, "evidence": "…",
        "title": "elided", "body": "nothing quoted",
    });
    let mut gates = FakeGates::concluding("unit", "pass", &json!([finding]));

    let report = check(&pr, &mut gates, &FakeModel::finding(&[]))?;

    let [gate] = report.gates() else {
        return Err("one gate is scheduled".into());
    };
    assert_eq!(gate.outcome, "gate-error:evidence-missing");
    assert_eq!(report.decision().conclusion, Conclusion::Failure);
    Ok(())
}

#[test]
fn every_gate_job_is_asked_for_its_plugins() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let mut gates = FakeGates::passing();

    check(&pr, &mut gates, &FakeModel::finding(&[]))?;

    assert_eq!(
        gates.calls,
        [("unit".to_owned(), GateJob::Build(assay_app::BuildJob::Test))]
    );
    Ok(())
}

#[test]
fn output_malformed_after_the_retry_degrades_the_role() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::answering([Ok(ModelOutcome::Malformed), Ok(ModelOutcome::Malformed)]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(model.call_count(), 2, "one retry");
    assert_eq!(report.review().status, ReviewStatus::Degraded);
    assert_eq!(
        report
            .decision()
            .degraded
            .iter()
            .map(|degraded| (degraded.role.as_str(), degraded.reason.as_str()))
            .collect::<Vec<_>>(),
        [("combined-micro", "malformed")]
    );
    Ok(())
}

#[test]
fn a_model_that_cannot_start_leaves_the_review_unavailable() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::answering([Err(NoCli)]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.review().status, ReviewStatus::Unavailable);
    assert_eq!(
        report.review().note.as_deref(),
        Some("claude could not be started")
    );
    assert_eq!(
        report
            .decision()
            .reasons
            .iter()
            .map(|reason| (reason.code, reason.detail.as_str()))
            .collect::<Vec<_>>(),
        [(ReasonCode::LlmDegraded, "infra-llm-unavailable")],
        "under the default `warn` policy the decision says the LLM stage was unavailable"
    );
    Ok(())
}

#[test]
fn a_session_with_another_tool_is_not_used() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel {
        tools: Some(&["Read", "Grep", "Glob", "Bash"]),
        ..FakeModel::finding(&[unwrap_finding()])
    };

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.review().status, ReviewStatus::Unavailable);
    assert_eq!(
        report.review().note.as_deref(),
        Some("the session was not confined to Read, Grep and Glob")
    );
    assert_eq!(report.review().findings, [], "its output is dropped");
    Ok(())
}

#[test]
fn a_session_whose_init_cannot_be_read_is_not_used() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel {
        tools: Some(&[]),
        ..FakeModel::finding(&[unwrap_finding()])
    };

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.review().status, ReviewStatus::Unavailable);
    assert_eq!(
        report.review().note.as_deref(),
        Some(
            "the session's init could not be read: the transcript does not start with a \
             system/init message"
        )
    );
    Ok(())
}

#[test]
fn a_call_that_exhausts_its_budget_degrades_the_role_without_a_retry() -> Result<(), Box<dyn Error>>
{
    let pr = dirty_tree();
    let model = FakeModel::answering([Ok(ModelOutcome::BudgetExhausted)]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(model.call_count(), 1);
    assert_eq!(report.review().status, ReviewStatus::Degraded);
    assert_eq!(
        report
            .decision()
            .degraded
            .iter()
            .map(|degraded| (degraded.role.as_str(), degraded.reason.as_str()))
            .collect::<Vec<_>>(),
        [("combined-micro", "cap")]
    );
    Ok(())
}

#[test]
fn a_retry_that_cannot_start_is_unavailable_and_charges_both_attempts() -> Result<(), Box<dyn Error>>
{
    let pr = dirty_tree();
    let model = FakeModel::answering([Ok(ModelOutcome::Malformed), Err(NoCli)]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.review().status, ReviewStatus::Unavailable);
    assert_eq!(
        report.review().cost.micros(),
        120_000 + 250_000,
        "the malformed attempt's cost plus the failed retry's cap"
    );
    Ok(())
}

#[test]
fn output_that_is_not_a_findings_object_completes_with_no_finding() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let model = FakeModel::answering([Ok(ModelOutcome::Ok(Structured::new(json!(["x"]))))]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.review().status, ReviewStatus::Completed);
    assert_eq!(report.review().findings, []);
    assert_eq!(
        report.review().note.as_deref(),
        Some("the model's document failed the findings schema")
    );
    Ok(())
}

/// The dirty tree with `src/lib.rs` counted as `added` lines.
fn sized_tree(added: u64) -> FakePr {
    let mut pr = dirty_tree();
    pr.changes = ChangedFiles::Listed(vec![FileChange::new(
        "src/lib.rs",
        Lines::Counted { added, removed: 1 },
    )]);
    pr
}

#[test]
fn a_lite_change_is_reviewed_by_combined_and_owes_a_verifier_for_a_major()
-> Result<(), Box<dyn Error>> {
    let pr = sized_tree(30);
    let model = FakeModel::finding(&[unwrap_finding()]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.tier(), Tier::Lite);
    assert_eq!(
        report.review().role.map(assay_app::Role::code),
        Some("combined")
    );
    assert_eq!(
        report
            .decision()
            .degraded
            .iter()
            .map(|degraded| (degraded.role.as_str(), degraded.reason.as_str()))
            .collect::<Vec<_>>(),
        [("verifier", "unavailable")],
        "no verifier runs locally, so the decision says it failed"
    );
    Ok(())
}

#[test]
fn a_full_change_reports_the_roster_and_coordinator_local_mode_does_not_run()
-> Result<(), Box<dyn Error>> {
    let pr = sized_tree(200);
    let model = FakeModel::finding(&[unwrap_finding()]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(report.tier(), Tier::Full);
    let degraded: Vec<(&str, &str)> = report
        .decision()
        .degraded
        .iter()
        .map(|degraded| (degraded.role.as_str(), degraded.reason.as_str()))
        .collect();
    for owed in [
        ("bugs", "missing"),
        ("coordinator", "unavailable"),
        ("verifier", "unavailable"),
    ] {
        assert!(degraded.contains(&owed), "{owed:?} in {degraded:?}");
    }
    Ok(())
}

#[test]
fn a_model_written_source_is_replaced_so_the_role_ceiling_holds() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let mut blocker = unwrap_finding();
    blocker["severity"] = json!("blocker");
    let model = FakeModel::answering([Ok(ModelOutcome::Ok(Structured::new(json!({
        "schema": "assay.findings/1",
        "source": {"kind": "reviewer", "name": "x", "version": "x", "role": "combined",
                   "run_key": "x"},
        "findings": [blocker],
    }))))]);

    let report = check(&pr, &mut FakeGates::passing(), &model)?;

    assert_eq!(
        report
            .review()
            .findings
            .iter()
            .map(|finding| finding.finding.severity)
            .collect::<Vec<_>>(),
        [Severity::Major],
        "combined-micro's findings are capped at major whatever role the model claims"
    );
    Ok(())
}

#[test]
fn a_context_pack_holding_a_nul_is_not_sent() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let setup = Setup::of(&pr)?;
    let mut patches = FakePatches::lib();
    if let Some(text) = lib_text(&mut patches) {
        text.push('\0');
    }
    let model = FakeModel::finding(&[]);
    let ports = CheckPorts {
        objects: &pr,
        patches: &patches,
        gates: &mut FakeGates::passing(),
        model: &model,
        events: &mut NoEvents,
    };

    let report = check_local(&setup.request(), ports, &validator()?)?;

    assert_eq!(model.call_count(), 0);
    assert_eq!(report.review().status, ReviewStatus::Unavailable);
    assert_eq!(
        report.review().note.as_deref(),
        Some("the context pack is not a valid prompt")
    );
    Ok(())
}

/// The text of `patches`' `src/lib.rs` patch.
pub(crate) fn lib_text(patches: &mut FakePatches) -> Option<&mut String> {
    patches
        .by_path
        .get_mut("src/lib.rs")
        .and_then(|patch| patch.text.as_mut())
}

/// The prompt of the one model call a check over `patches` makes.
pub(crate) fn prompt_with(patches: &FakePatches, pr: &FakePr) -> Result<String, Box<dyn Error>> {
    let setup = Setup::of(pr)?;
    let model = FakeModel::finding(&[]);
    let ports = CheckPorts {
        objects: pr,
        patches,
        gates: &mut FakeGates::passing(),
        model: &model,
        events: &mut NoEvents,
    };
    check_local(&setup.request(), ports, &validator()?)?;
    let calls = model.calls.borrow();
    let call = calls.first().ok_or("the review ran")?;
    Ok(call.user_prompt.as_str().to_owned())
}

#[test]
fn the_diff_is_sent_whole_at_its_cap_and_left_out_one_byte_over() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let sized = |len: usize| {
        let mut patches = FakePatches::lib();
        if let Some(text) = lib_text(&mut patches) {
            let pad = len.saturating_sub(text.len());
            text.push_str(&" ".repeat(pad));
        }
        patches
    };

    let at_cap = prompt_with(&sized(MAX_DIFF_BYTES), &pr)?;
    let over = prompt_with(&sized(MAX_DIFF_BYTES + 1), &pr)?;

    assert!(
        at_cap.contains("diff --git a/src/lib.rs"),
        "a diff at the cap is sent"
    );
    assert!(
        !at_cap.contains("not shown"),
        "nothing is left out at the cap"
    );
    assert!(
        !over.contains("diff --git a/src/lib.rs"),
        "a diff over the cap is not cut"
    );
    assert!(
        over.contains("[1 more file not shown: the diff stops at 122880 bytes]"),
        "the pack says what it left out: {over:?}"
    );
    Ok(())
}

#[test]
fn the_file_list_is_whole_at_its_cap_and_cut_one_byte_over() -> Result<(), Box<dyn Error>> {
    // Each listed line is `M <path>\n`; one file whose line is exactly the cap.
    let listed = |len: usize| -> Result<String, Box<dyn Error>> {
        let path = format!(
            "src/{}.rs",
            "a".repeat(len.saturating_sub("M src/.rs\n".len()))
        );
        let mut pr = dirty_tree();
        pr.write(BASE, &path, b"x\n");
        pr.write(HEAD, &path, b"y\n");
        pr.changes = ChangedFiles::Listed(vec![FileChange::new(
            path.as_str(),
            Lines::Counted {
                added: 1,
                removed: 1,
            },
        )]);
        prompt_with(&FakePatches::default(), &pr)
    };

    let at_cap = listed(MAX_FILE_LIST_BYTES)?;
    let over = listed(MAX_FILE_LIST_BYTES + 1)?;

    assert!(
        at_cap.starts_with("Changed files (1):\nM src/aaa"),
        "{:?}",
        at_cap.get(..40)
    );
    assert!(!at_cap.contains("not listed"));
    assert!(
        over.starts_with("Changed files (1):\n[1 more file not listed]\n"),
        "{:?}",
        over.get(..60)
    );
    Ok(())
}

#[test]
fn the_pack_names_each_file_s_status_from_both_trees() -> Result<(), Box<dyn Error>> {
    let mut pr = dirty_tree();
    pr.write(HEAD, "src/new.rs", b"pub fn new() {}\n");
    pr.write(BASE, "src/old.rs", b"pub fn old() {}\n");
    pr.write(BASE, "src/was.rs", b"pub fn moved() {}\n");
    pr.write(HEAD, "src/moved.rs", b"pub fn moved() {}\n");
    let one = Lines::Counted {
        added: 1,
        removed: 0,
    };
    pr.changes = ChangedFiles::Listed(vec![
        FileChange::new("src/new.rs", one),
        FileChange::new("src/old.rs", one),
        FileChange::new("src/moved.rs", one).renamed_from("src/was.rs"),
    ]);

    let prompt = prompt_with(&FakePatches::default(), &pr)?;

    assert!(
        prompt.starts_with(
            "Changed files (3):\nA src/new.rs\nD src/old.rs\nR src/was.rs -> src/moved.rs\n"
        ),
        "{prompt:?}"
    );
    Ok(())
}

#[test]
fn a_patch_port_failure_fails_the_check() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let setup = Setup::of(&pr)?;
    let patches = FakePatches {
        failing: true,
        ..FakePatches::lib()
    };
    let ports = CheckPorts {
        objects: &pr,
        patches: &patches,
        gates: &mut FakeGates::passing(),
        model: &FakeModel::default(),
        events: &mut NoEvents,
    };

    let result = check_local(&setup.request(), ports, &validator()?);

    assert_matches!(result, Err(CheckError::Patches(_)));
    Ok(())
}

#[test]
fn a_patch_list_of_the_wrong_length_fails_the_check() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let setup = Setup::of(&pr)?;
    let patches = FakePatches {
        short: true,
        ..FakePatches::lib()
    };
    let ports = CheckPorts {
        objects: &pr,
        patches: &patches,
        gates: &mut FakeGates::passing(),
        model: &FakeModel::default(),
        events: &mut NoEvents,
    };

    let result = check_local(&setup.request(), ports, &validator()?);

    assert_matches!(
        result,
        Err(CheckError::PatchCount {
            asked: 1,
            returned: 0
        })
    );
    Ok(())
}

#[test]
fn an_unreadable_changed_file_fails_the_check() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let setup = Setup::of(&pr)?;
    let objects = Unreadable {
        pr: &pr,
        path: "src/lib.rs",
        short: false,
    };
    let ports = CheckPorts {
        objects: &objects,
        patches: &FakePatches::lib(),
        gates: &mut FakeGates::passing(),
        model: &FakeModel::default(),
        events: &mut NoEvents,
    };

    let result = check_local(&setup.request(), ports, &validator()?);

    assert_matches!(result, Err(CheckError::Objects(_)));
    Ok(())
}

#[test]
fn a_short_read_of_the_changed_files_fails_the_check() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let setup = Setup::of(&pr)?;
    let objects = Unreadable {
        pr: &pr,
        path: "src/lib.rs",
        short: true,
    };
    let ports = CheckPorts {
        objects: &objects,
        patches: &FakePatches::lib(),
        gates: &mut FakeGates::passing(),
        model: &FakeModel::default(),
        events: &mut NoEvents,
    };

    let result = check_local(&setup.request(), ports, &validator()?);

    assert_matches!(result, Err(CheckError::ShortRead { .. }));
    Ok(())
}

#[test]
fn a_rubric_that_does_not_parse_fails_the_check_first() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let mut setup = Setup::of(&pr)?;
    setup.rubric = b"version = \n".to_vec();
    let model = FakeModel::default();
    let mut gates = FakeGates::passing();
    let ports = CheckPorts {
        objects: &pr,
        patches: &FakePatches::lib(),
        gates: &mut gates,
        model: &model,
        events: &mut NoEvents,
    };

    let result = check_local(&setup.request(), ports, &validator()?);

    assert_matches!(result, Err(CheckError::Rubric { line: 1 }));
    assert_eq!(gates.calls, [], "nothing runs before the rubric is read");
    Ok(())
}

#[test]
fn ci_refuses_the_local_report_as_every_contract() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let report = check(
        &pr,
        &mut FakeGates::passing(),
        &FakeModel::finding(&[unwrap_finding()]),
    )?;
    let json = report.to_json()?;
    let validator = validator()?;

    let read: Vec<(Contract, bool)> = Contract::ALL
        .into_iter()
        .map(|contract| {
            (
                contract,
                matches!(
                    validator.read(contract, &json),
                    Err(ReadContractError::UnknownSchema { .. })
                ),
            )
        })
        .collect();

    // Every CI reader goes through the validator (03 preamble), so no CI path can accept the
    // file local mode writes: it is advisory and never a trust anchor (R-10, P9).
    assert_eq!(
        read,
        Contract::ALL.map(|contract| (contract, true)),
        "the local report is no contract CI reads"
    );
    let value: Value = serde_json::from_slice(&json)?;
    assert_eq!(
        (&value["schema"], &value["advisory"]),
        (&json!("assay.local-check/2"), &json!(true))
    );
    Ok(())
}

#[test]
fn the_terminal_report_shows_no_control_character_from_model_text() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let mut finding = unwrap_finding();
    finding["title"] = json!("unwrap \u{1b}[2Jcleared\u{202e}reversed");
    let report = check(
        &pr,
        &mut FakeGates::passing(),
        &FakeModel::finding(&[finding]),
    )?;

    let text = report.render();

    assert!(
        text.contains("major src/lib.rs:2 unwrap \u{fffd}[2Jcleared\u{fffd}reversed"),
        "{text}"
    );
    assert!(
        text.chars().all(|c| c == '\n' || !c.is_control()),
        "only the report's own line breaks: {text:?}"
    );
    assert!(text.starts_with("assay check --local: advisory, CI never reads this report\n"));
    Ok(())
}
