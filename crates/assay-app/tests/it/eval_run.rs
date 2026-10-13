//! Running an evaluation (04 §8): k = 3 runs per case through a model runner, scored, and
//! charged to the separate eval ledger, which refuses once it cannot fit a call.

use std::error::Error;
use std::path::Path;

use assay_app::{
    CaseKind, ClassMap, EVAL_MONTHLY_CAP, EVAL_ROLE, EvalError, EvalMonth, ModelOutcome,
    ModelResult, OutputProblem, RunScore, Split, Unscorable, read_eval_corpus, run_eval,
};
use assay_domain::Aeu;
use serde_json::json;

use crate::check_fakes::session;
use crate::eval_fakes::{
    Answer, CALL_COST, CORPUS_DIR, DEFECT_JSON, FakeLedger, LedgerDown, QUIET_JSON, ScriptedModel,
    corpus, ended, hit, quiet, ran, request, stray, today,
};
use crate::specialists_fakes::bundle;

type TestResult = Result<(), Box<dyn Error>>;

/// The worst case of one call: two attempts at the `combined` cap.
fn worst_call() -> Aeu {
    EVAL_ROLE.cap().saturating_add(EVAL_ROLE.cap())
}

fn month() -> Result<EvalMonth, Box<dyn Error>> {
    Ok(EvalMonth::of(today()?))
}

#[test]
fn a_defect_found_in_every_run_scores_three_detections() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([hit(), hit(), hit()]);
    let mut ledger = FakeLedger::default();

    let report = run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let [case] = report.cases() else {
        return Err("one case expected".into());
    };
    assert_eq!(case.runs(), &[RunScore::Detected; 3]);
    assert_eq!((case.kind(), case.detected()), (CaseKind::Defect, 3));
    assert_eq!(report.spent(), Aeu::from_micros(300_000));
    assert_eq!(
        (report.split(), report.bundle()),
        (Split::Dev, bundle.hash())
    );
    Ok(())
}

#[test]
fn each_call_reads_only_the_case_tree_and_sees_the_diff_but_not_the_labels() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([]);

    run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let calls = model.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        assert_eq!(call.add_dir, Path::new(CORPUS_DIR).join("off-by-one/tree"));
        assert_eq!(call.budget, EVAL_ROLE.cap());
        assert!(
            call.user_prompt.as_str().starts_with("Diff:\ndiff --git"),
            "{call:?}"
        );
        assert!(
            !call.user_prompt.as_str().contains("start_line"),
            "labels leaked"
        );
    }
    Ok(())
}

#[test]
fn a_defect_missed_by_some_runs_counts_only_the_detections() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([hit(), stray(), quiet()]);

    let report = run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let runs: Vec<_> = report
        .cases()
        .iter()
        .flat_map(|case| *case.runs())
        .collect();
    assert_eq!(
        runs,
        [RunScore::Detected, RunScore::Missed, RunScore::Missed]
    );
    Ok(())
}

#[test]
fn a_finding_on_a_true_negative_is_noise() -> TestResult {
    let corpus = corpus(&[("clean", QUIET_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([stray(), quiet(), quiet()]);

    let report = run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let [case] = report.cases() else {
        return Err("one case expected".into());
    };
    assert_eq!(
        case.runs(),
        &[
            RunScore::Noisy { findings: 1 },
            RunScore::Quiet,
            RunScore::Quiet
        ]
    );
    assert_eq!((case.kind(), case.noisy()), (CaseKind::TrueNegative, 1));
    assert_eq!((report.noise().hits(), report.noise().n()), (1, 3));
    Ok(())
}

#[test]
fn unscorable_runs_count_as_misses_and_as_noise() -> TestResult {
    let corpus = corpus(&[("clean", QUIET_JSON), ("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let timeout = Answer::Ran(ended(ModelOutcome::Timeout));
    let model = ScriptedModel::new([timeout.clone(), quiet(), quiet(), timeout, hit(), hit()]);

    let report = run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let counts: Vec<(u32, u32)> = report
        .cases()
        .iter()
        .map(|case| (case.noisy(), case.detected()))
        .collect();
    assert_eq!(counts, [(1, 0), (0, 2)]);
    Ok(())
}

/// The score of the first run of a defect case whose first call answers `answers`.
fn first_score(answers: Vec<Answer>) -> Result<RunScore, Box<dyn Error>> {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new(answers);

    let report = run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let first = report.cases().first().ok_or("no case")?;
    Ok(first.runs()[0])
}

#[test]
fn each_way_a_call_can_end_unscored_is_named() -> TestResult {
    let malformed = || Answer::Ran(ended(ModelOutcome::Malformed));
    let cases = [
        (vec![Answer::Fails], Unscorable::CallFailed),
        (vec![malformed(), malformed()], Unscorable::Malformed),
        (
            vec![Answer::Ran(ended(ModelOutcome::BudgetExhausted))],
            Unscorable::BudgetExhausted,
        ),
        (
            vec![Answer::Ran(ended(ModelOutcome::TurnsExhausted))],
            Unscorable::TurnsExhausted,
        ),
        (
            vec![Answer::Ran(ended(ModelOutcome::Timeout))],
            Unscorable::Timeout,
        ),
        (
            vec![Answer::Ran(ended(ModelOutcome::QuotaExhausted))],
            Unscorable::QuotaExhausted,
        ),
        (
            vec![Answer::Ran(ran(json!({"verdict": "ok"})))],
            Unscorable::Output(OutputProblem::NotADocument),
        ),
    ];
    for (answers, reason) in cases {
        assert_eq!(
            first_score(answers)?,
            RunScore::Unscorable(reason),
            "{reason:?}"
        );
    }
    Ok(())
}

#[test]
fn a_malformed_answer_is_retried_once_and_the_retry_scored() -> TestResult {
    let score = first_score(vec![Answer::Ran(ended(ModelOutcome::Malformed)), hit()])?;

    assert_eq!(score, RunScore::Detected);
    Ok(())
}

#[test]
fn an_unconfined_session_is_not_scored_even_when_it_found_the_defect() -> TestResult {
    let Answer::Ran(found) = hit() else {
        return Err("hit runs".into());
    };
    let unconfined = ModelResult {
        init: session(&["Read", "Grep", "Glob", "Bash"]),
        ..found
    };

    let score = first_score(vec![Answer::Ran(unconfined)])?;

    assert_eq!(score, RunScore::Unscorable(Unscorable::Unconfined));
    Ok(())
}

#[test]
fn a_failed_call_is_charged_its_cap() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([Answer::Fails]);
    let mut ledger = FakeLedger::default();

    run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let charged: Vec<Aeu> = ledger.charges.iter().map(|(_, cost)| *cost).collect();
    assert_eq!(charged, [EVAL_ROLE.cap(), CALL_COST, CALL_COST]);
    Ok(())
}

#[test]
fn a_retry_that_fails_is_charged_the_first_cost_and_the_cap() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([Answer::Ran(ended(ModelOutcome::Malformed)), Answer::Fails]);
    let mut ledger = FakeLedger::default();

    run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let first = ledger.charges.first().map(|(_, cost)| *cost);
    assert_eq!(first, Some(CALL_COST.saturating_add(EVAL_ROLE.cap())));
    Ok(())
}

#[test]
fn every_charge_goes_to_the_month_of_today() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let mut ledger = FakeLedger::default();

    let report = run_eval(
        &ScriptedModel::new([]),
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let months: Vec<String> = ledger
        .charges
        .iter()
        .map(|(month, _)| month.to_string())
        .collect();
    assert_eq!(months, ["2026-10"; 3]);
    assert_eq!(report.month(), month()?);
    Ok(())
}

#[test]
fn refuses_to_run_when_the_eval_ledger_is_exhausted() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([]);
    let mut ledger = FakeLedger::with_spent(EVAL_MONTHLY_CAP);

    let result = run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    );

    assert!(
        matches!(
            result,
            Err(EvalError::Exhausted { spent, cap, .. })
                if spent == EVAL_MONTHLY_CAP && cap == EVAL_MONTHLY_CAP
        ),
        "{result:?}"
    );
    assert!(model.calls().is_empty(), "no call may start");
    assert!(ledger.charges.is_empty(), "nothing charged");
    Ok(())
}

#[test]
fn runs_a_call_when_exactly_its_worst_case_is_left() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([]);
    let opening = Aeu::from_micros(EVAL_MONTHLY_CAP.micros() - worst_call().micros());
    let mut ledger = FakeLedger::with_spent(opening);

    let result = run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    );

    // The first call fits exactly; after its cost the second no longer does.
    assert!(
        matches!(result, Err(EvalError::Exhausted { spent, .. }) if spent == opening.saturating_add(CALL_COST)),
        "{result:?}"
    );
    assert_eq!(model.calls().len(), 1);
    assert_eq!(ledger.charges, [(month()?, CALL_COST)]);
    Ok(())
}

#[test]
fn refuses_a_call_one_micro_aeu_short_of_its_worst_case() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([]);
    let opening = Aeu::from_micros(EVAL_MONTHLY_CAP.micros() - worst_call().micros() + 1);
    let mut ledger = FakeLedger::with_spent(opening);

    let result = run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    );

    assert!(
        matches!(result, Err(EvalError::Exhausted { .. })),
        "{result:?}"
    );
    assert!(model.calls().is_empty(), "no call may start");
    Ok(())
}

#[test]
fn an_unreadable_eval_ledger_stops_the_run_before_any_call() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([]);
    let mut ledger = FakeLedger {
        unreadable: true,
        ..FakeLedger::default()
    };

    let result = run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    );

    assert!(
        matches!(
            result,
            Err(EvalError::Ledger {
                source: LedgerDown,
                ..
            })
        ),
        "{result:?}"
    );
    assert!(model.calls().is_empty(), "no call may start");
    Ok(())
}

#[test]
fn a_charge_that_fails_stops_the_run_after_that_call() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([]);
    let mut ledger = FakeLedger {
        charges_fail: true,
        ..FakeLedger::default()
    };

    let result = run_eval(
        &model,
        &mut ledger,
        &request(&corpus, &bundle, &map, today()?),
    );

    assert!(
        matches!(
            result,
            Err(EvalError::Ledger {
                source: LedgerDown,
                ..
            })
        ),
        "{result:?}"
    );
    assert_eq!(model.calls().len(), 1);
    Ok(())
}

#[test]
fn refuses_an_empty_split() -> TestResult {
    let corpus = read_eval_corpus(Split::Dev, &[])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());

    let result = run_eval(
        &ScriptedModel::new([]),
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    );

    assert!(matches!(result, Err(EvalError::NoCases)), "{result:?}");
    Ok(())
}

#[test]
fn an_aliased_class_counts_as_a_detection() -> TestResult {
    let corpus = corpus(&[("off-by-one", DEFECT_JSON)])?;
    let bundle = bundle()?;
    let map =
        ClassMap::identity().with_alias(assay_domain::Class::Logic, assay_domain::Class::Tests);
    let tests_finding = crate::eval_fakes::finding("src/lib.rs", 40, "tests");
    let answer = || {
        Answer::Ran(ran(crate::eval_fakes::output(std::slice::from_ref(
            &tests_finding,
        ))))
    };
    let model = ScriptedModel::new([answer(), answer(), answer()]);

    let report = run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let detected: Vec<u32> = report
        .cases()
        .iter()
        .map(assay_app::CaseOutcome::detected)
        .collect();
    assert_eq!(detected, [3]);
    Ok(())
}

#[test]
fn recall_is_reported_per_authoring_model() -> TestResult {
    let by_model = DEFECT_JSON.replace("\"human\"", "\"claude-opus-5-5\"");
    let corpus = corpus(&[("by-human", DEFECT_JSON), ("by-model", &by_model)])?;
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new([hit(), hit(), hit(), hit(), quiet(), quiet()]);

    let report = run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(&corpus, &bundle, &map, today()?),
    )?;

    let recall: Vec<(String, u32, u32)> = report
        .recall_by_author()
        .into_iter()
        .map(|(author, share)| (author.to_string(), share.hits(), share.n()))
        .collect();
    assert_eq!(
        recall,
        [
            ("human".to_owned(), 3, 3),
            ("claude-opus-5-5".to_owned(), 1, 3),
        ]
    );
    Ok(())
}
