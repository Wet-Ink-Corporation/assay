//! 04 §8's accept rule: no case flips from at least 2/3 detected to at most 1/3, and the
//! true-negative noise rate stays within the baseline's Wilson upper bound.

use std::error::Error;

use assay_app::{
    ClassMap, CompareError, CorpusEntryKind, EvalCorpus, EvalReport, NoiseCheck, Split, compare,
    read_eval_corpus, run_eval,
};

use crate::eval_fakes::{
    Answer, DEFECT_JSON, DIFF, FakeLedger, QUIET_JSON, ScriptedModel, case_entries, corpus, hit,
    quiet, request, stray, timed_out, today,
};
use crate::specialists_fakes::bundle;

type TestResult = Result<(), Box<dyn Error>>;

/// `corpus` evaluated with the scripted `answers`, in case-then-run order.
fn evaluate(corpus: &EvalCorpus, answers: Vec<Answer>) -> Result<EvalReport, Box<dyn Error>> {
    let (bundle, map) = (bundle()?, ClassMap::identity());
    let model = ScriptedModel::new(answers);
    Ok(run_eval(
        &model,
        &mut FakeLedger::default(),
        &request(corpus, &bundle, &map, today()?),
    )?)
}

/// A corpus of one defect case (`defect`) and one true-negative case (`clean`).
fn pair() -> Result<EvalCorpus, Box<dyn Error>> {
    corpus(&[("clean", QUIET_JSON), ("defect", DEFECT_JSON)])
}

/// Answers for [`pair`]: the true negative's three runs, then the defect's.
fn answers(clean: [fn() -> Answer; 3], defect: [fn() -> Answer; 3]) -> Vec<Answer> {
    clean.iter().chain(&defect).map(|answer| answer()).collect()
}

#[test]
fn accepts_when_nothing_flips_and_noise_holds() -> TestResult {
    let corpus = pair()?;
    let baseline = evaluate(&corpus, answers([quiet; 3], [hit, hit, quiet]))?;
    let candidate = evaluate(&corpus, answers([quiet; 3], [hit, hit, hit]))?;

    let acceptance = compare(&baseline, &candidate)?;

    assert!(acceptance.accepted(), "{acceptance:?}");
    assert!(acceptance.flips().is_empty(), "{acceptance:?}");
    Ok(())
}

#[test]
fn rejects_a_case_that_flips_from_two_thirds_to_one_third() -> TestResult {
    let corpus = pair()?;
    let baseline = evaluate(&corpus, answers([quiet; 3], [hit, hit, quiet]))?;
    let candidate = evaluate(&corpus, answers([quiet; 3], [hit, quiet, quiet]))?;

    let acceptance = compare(&baseline, &candidate)?;

    assert!(!acceptance.accepted(), "{acceptance:?}");
    assert_eq!(acceptance.flips(), ["defect"]);
    Ok(())
}

#[test]
fn three_to_two_thirds_is_not_a_flip() -> TestResult {
    let corpus = pair()?;
    let baseline = evaluate(&corpus, answers([quiet; 3], [hit, hit, hit]))?;
    let candidate = evaluate(&corpus, answers([quiet; 3], [hit, hit, quiet]))?;

    let acceptance = compare(&baseline, &candidate)?;

    assert!(acceptance.accepted(), "{acceptance:?}");
    Ok(())
}

#[test]
fn one_third_to_none_is_not_a_flip() -> TestResult {
    let corpus = pair()?;
    let baseline = evaluate(&corpus, answers([quiet; 3], [hit, quiet, quiet]))?;
    let candidate = evaluate(&corpus, answers([quiet; 3], [quiet; 3]))?;

    let acceptance = compare(&baseline, &candidate)?;

    assert!(acceptance.flips().is_empty(), "{acceptance:?}");
    Ok(())
}

/// Ten true-negative cases and nothing else.
fn ten_clean() -> Result<EvalCorpus, Box<dyn Error>> {
    let entries: Vec<_> = (0..10)
        .flat_map(|index| case_entries(&format!("clean-{index}"), QUIET_JSON, DIFF))
        .collect();
    Ok(read_eval_corpus(Split::Dev, &entries)?)
}

/// Thirty runs of which the first `noisy` report a finding.
fn noisy_runs(noisy: usize) -> Vec<Answer> {
    (0..30)
        .map(|run| if run < noisy { stray() } else { quiet() })
        .collect()
}

#[test]
fn noise_within_the_wilson_margin_is_accepted() -> TestResult {
    // 0 of 30 has a 95% Wilson upper bound of 0.1135; 3 of 30 is 0.1.
    let corpus = ten_clean()?;
    let baseline = evaluate(&corpus, noisy_runs(0))?;
    let candidate = evaluate(&corpus, noisy_runs(3))?;

    let acceptance = compare(&baseline, &candidate)?;

    assert!(
        matches!(acceptance.noise(), NoiseCheck::Within { .. }),
        "{acceptance:?}"
    );
    assert!(acceptance.accepted(), "{acceptance:?}");
    assert_eq!(
        (
            acceptance.candidate_noise().hits(),
            acceptance.candidate_noise().n()
        ),
        (3, 30)
    );
    Ok(())
}

#[test]
fn noise_past_the_wilson_margin_is_rejected() -> TestResult {
    // 4 of 30 is 0.133, over 0 of 30's upper bound of 0.1135.
    let corpus = ten_clean()?;
    let baseline = evaluate(&corpus, noisy_runs(0))?;
    let candidate = evaluate(&corpus, noisy_runs(4))?;

    let acceptance = compare(&baseline, &candidate)?;

    assert!(
        matches!(acceptance.noise(), NoiseCheck::Rose { .. }),
        "{acceptance:?}"
    );
    assert!(!acceptance.accepted(), "{acceptance:?}");
    Ok(())
}

#[test]
fn a_split_without_true_negatives_is_not_accepted() -> TestResult {
    let corpus = corpus(&[("defect", DEFECT_JSON)])?;
    let baseline = evaluate(&corpus, vec![hit(), hit(), hit()])?;
    let candidate = evaluate(&corpus, vec![hit(), hit(), hit()])?;

    let acceptance = compare(&baseline, &candidate)?;

    assert_eq!(acceptance.noise(), NoiseCheck::NoTrueNegatives);
    assert!(!acceptance.accepted(), "{acceptance:?}");
    Ok(())
}

#[test]
fn refuses_reports_over_different_cases() -> TestResult {
    let baseline = evaluate(&pair()?, Vec::new())?;
    let candidate = evaluate(&corpus(&[("clean", QUIET_JSON)])?, Vec::new())?;

    assert_eq!(
        compare(&baseline, &candidate),
        Err(CompareError::CaseSetDiffers {
            case: "defect".to_owned()
        })
    );
    assert_eq!(
        compare(&candidate, &baseline),
        Err(CompareError::CaseSetDiffers {
            case: "defect".to_owned()
        })
    );
    Ok(())
}

#[test]
fn refuses_reports_whose_cases_differ_by_id() -> TestResult {
    let baseline = evaluate(&corpus(&[("alpha", QUIET_JSON)])?, Vec::new())?;
    let candidate = evaluate(&corpus(&[("beta", QUIET_JSON)])?, Vec::new())?;

    assert_eq!(
        compare(&baseline, &candidate),
        Err(CompareError::CaseSetDiffers {
            case: "alpha".to_owned()
        })
    );
    Ok(())
}

#[test]
fn refuses_reports_over_different_splits() -> TestResult {
    let entries = case_entries("clean", QUIET_JSON, DIFF);
    let baseline = evaluate(&read_eval_corpus(Split::Dev, &entries)?, Vec::new())?;
    let candidate = evaluate(&read_eval_corpus(Split::Test, &entries)?, Vec::new())?;

    assert_eq!(
        compare(&baseline, &candidate),
        Err(CompareError::SplitDiffers)
    );
    Ok(())
}

#[test]
fn refuses_a_baseline_with_an_unscorable_run_on_a_defect_case() -> TestResult {
    let corpus = pair()?;
    let baseline = evaluate(&corpus, answers([quiet; 3], [timed_out, quiet, quiet]))?;
    let candidate = evaluate(&corpus, answers([quiet; 3], [quiet; 3]))?;

    assert_eq!(
        compare(&baseline, &candidate),
        Err(CompareError::BaselineUnscorable {
            case: "defect".to_owned()
        })
    );
    Ok(())
}

#[test]
fn refuses_a_baseline_with_an_unscorable_run_on_a_true_negative() -> TestResult {
    let corpus = pair()?;
    let baseline = evaluate(&corpus, answers([quiet, timed_out, quiet], [hit; 3]))?;
    let candidate = evaluate(&corpus, answers([stray; 3], [hit; 3]))?;

    assert_eq!(
        compare(&baseline, &candidate),
        Err(CompareError::BaselineUnscorable {
            case: "clean".to_owned()
        })
    );
    Ok(())
}

#[test]
fn refuses_reports_whose_case_content_changed() -> TestResult {
    let before = corpus(&[("defect", DEFECT_JSON)])?;
    let entries = case_entries("defect", DEFECT_JSON, "diff --git a/x b/x\n");
    let after = read_eval_corpus(Split::Dev, &entries)?;
    let baseline = evaluate(&before, Vec::new())?;
    let candidate = evaluate(&after, Vec::new())?;

    assert_eq!(
        compare(&baseline, &candidate),
        Err(CompareError::CaseChanged {
            case: "defect".to_owned()
        })
    );
    Ok(())
}

#[test]
fn refuses_reports_whose_case_tree_changed() -> TestResult {
    let before = corpus(&[("defect", DEFECT_JSON)])?;
    let mut entries = case_entries("defect", DEFECT_JSON, DIFF);
    for entry in &mut entries {
        if entry.path == "defect/tree/src/lib.rs" {
            entry.kind = CorpusEntryKind::File(b"fn moved() {}\n".to_vec());
        }
    }
    let after = read_eval_corpus(Split::Dev, &entries)?;
    let baseline = evaluate(&before, Vec::new())?;
    let candidate = evaluate(&after, Vec::new())?;

    assert_eq!(
        compare(&baseline, &candidate),
        Err(CompareError::CaseChanged {
            case: "defect".to_owned()
        })
    );
    Ok(())
}
