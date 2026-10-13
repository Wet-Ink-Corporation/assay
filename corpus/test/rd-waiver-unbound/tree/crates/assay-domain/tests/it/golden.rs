//! The golden decision corpus (`corpus/golden/`, 02 §7): every case's `inputs.json`, decided
//! with `rubric/v1.toml`, gives exactly the bytes of its `decision.json`. The `assay-self` gate
//! replays the same corpus.

use std::collections::BTreeSet;
use std::error::Error;

use assay_domain::{DecisionInputs, ReasonCode, decide};

use crate::support::{golden_cases, rubric_v1};

/// The written form of a decision: pretty JSON with a final newline.
fn written(inputs: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let inputs: DecisionInputs = serde_json::from_slice(inputs)?;
    let mut bytes = serde_json::to_vec_pretty(&decide(&rubric_v1()?, &inputs))?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[test]
fn every_golden_case_decides_to_its_recorded_bytes() -> Result<(), Box<dyn Error>> {
    let cases = golden_cases()?;
    assert!(!cases.is_empty(), "corpus/golden/ holds no cases");

    let mut differing = Vec::new();
    for (name, case) in &cases {
        let actual = written(&case.inputs).map_err(|e| format!("{name}: {e}"))?;
        if actual != case.decision {
            differing.push(format!("{name}:\n{}", String::from_utf8_lossy(&actual)));
        }
    }

    assert!(differing.is_empty(), "{}", differing.join("\n"));
    Ok(())
}

#[test]
fn the_golden_corpus_exercises_every_rubric_reason() -> Result<(), Box<dyn Error>> {
    let mut seen = BTreeSet::new();
    for case in golden_cases()?.values() {
        let decision: serde_json::Value = serde_json::from_slice(&case.decision)?;
        let codes = decision["reasons"]
            .as_array()
            .ok_or("reasons is an array")?;
        seen.extend(
            codes
                .iter()
                .filter_map(|r| r["code"].as_str())
                .map(str::to_owned),
        );
    }

    let rubric_reasons = [
        ReasonCode::GateFailed,
        ReasonCode::GateError,
        ReasonCode::GateSkipped,
        ReasonCode::BaseNotProtected,
        ReasonCode::AmbiguousHead,
        ReasonCode::NeedsSplit,
        ReasonCode::TrustPathUnapproved,
        ReasonCode::WeighInPendingTriage,
        ReasonCode::BlockerVerified,
        ReasonCode::MajorsOverThreshold,
        ReasonCode::BudgetDeferred,
        ReasonCode::SlotBusy,
        ReasonCode::InfraLlmUnavailable,
        ReasonCode::RequiredRoleDegraded,
        ReasonCode::LlmDegraded,
        ReasonCode::Override,
    ];
    let missing: Vec<&str> = rubric_reasons
        .iter()
        .map(|code| code.code())
        .filter(|code| !seen.contains(*code))
        .collect();

    assert_eq!(missing, Vec::<&str>::new(), "reasons no golden case covers");
    Ok(())
}
