//! A scripted model runner that records its calls, and builders for the verifier's inputs.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    BundleFiles, CarriedFinding, CarriedParts, FindingRef, InitReadError, ModelCall, ModelOutcome,
    ModelResult, ModelRunner, Placement, PromptBundle, Structured, ValidFinding, VerifierTier,
    VerifyRequest,
};
use assay_domain::{Aeu, Class, Confidence, Finding, FindingKind, Severity};
use serde_json::{Value, json};

/// The repository under review.
pub(crate) const REPOSITORY: &str = "Wet-Ink-Corporation/assay";

/// The runner could not make the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unreachable;

impl fmt::Display for Unreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the model could not be reached")
    }
}

impl Error for Unreachable {}

/// Answers each call with the next scripted answer, `Err(Unreachable)` once none is left, and
/// records every call it was given.
#[derive(Debug)]
pub(crate) struct Scripted {
    answers: RefCell<VecDeque<Result<ModelResult, Unreachable>>>,
    pub(crate) calls: RefCell<Vec<ModelCall>>,
}

impl Scripted {
    pub(crate) fn new(answers: impl IntoIterator<Item = Result<ModelResult, Unreachable>>) -> Self {
        Self {
            answers: RefCell::new(answers.into_iter().collect()),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// A runner that answers every call with `outcome` at `micros`.
    pub(crate) fn answering(outcomes: impl IntoIterator<Item = (ModelOutcome, u64)>) -> Self {
        Self::new(
            outcomes
                .into_iter()
                .map(|(outcome, micros)| Ok(result(outcome, micros))),
        )
    }

    pub(crate) fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl ModelRunner for Scripted {
    type Error = Unreachable;

    fn run(&self, call: &ModelCall) -> Result<ModelResult, Unreachable> {
        // A copy: the test inspects the calls after `verify` returns.
        self.calls.borrow_mut().push(call.clone());
        self.answers
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(Unreachable))
    }
}

pub(crate) fn result(outcome: ModelOutcome, micros: u64) -> ModelResult {
    ModelResult {
        outcome,
        cost: Aeu::from_micros(micros),
        rate_limits: Vec::new(),
        init: Err(InitReadError::Missing),
    }
}

/// Structured output holding `value`.
pub(crate) fn output(value: Value) -> ModelOutcome {
    ModelOutcome::Ok(Structured::new(value))
}

/// A well-formed verdict.
pub(crate) fn verdict(code: &str, reason: &str) -> ModelOutcome {
    output(json!({"verdict": code, "reason": reason}))
}

/// The bundle with short stand-in texts.
pub(crate) fn bundle() -> Result<PromptBundle, Box<dyn Error>> {
    Ok(PromptBundle::new(BundleFiles {
        system: "SYSTEM".to_owned(),
        bugs: "BUGS".to_owned(),
        security: "SECURITY".to_owned(),
        design: "DESIGN".to_owned(),
        meta: "META".to_owned(),
        combined: "COMBINED".to_owned(),
        combined_micro: "MICRO".to_owned(),
        verifier: "VERIFIER".to_owned(),
        verifier_carried: "VERIFIER-CARRIED".to_owned(),
        coordinator: "COORDINATOR".to_owned(),
    })?)
}

/// A request to verify `findings` in `tier`, nothing carried.
pub(crate) fn request<'a>(
    tier: VerifierTier,
    findings: &'a [ValidFinding],
    bundle: &'a PromptBundle,
) -> VerifyRequest<'a> {
    request_carrying(tier, findings, &[], bundle)
}

/// A request to verify `findings` and the `carried` findings of earlier runs in `tier`.
pub(crate) fn request_carrying<'a>(
    tier: VerifierTier,
    findings: &'a [ValidFinding],
    carried: &'a [CarriedFinding],
    bundle: &'a PromptBundle,
) -> VerifyRequest<'a> {
    VerifyRequest {
        tier,
        repository: REPOSITORY,
        findings,
        carried,
        bundle,
        system_prompt_file: PathBuf::from("/base/plugin/prompts/v1/system.md"),
        add_dir: PathBuf::from("/work/pr"),
        timeout: Duration::from_secs(300),
    }
}

/// A validated finding of `severity`, the `index`th of the run's only document.
pub(crate) fn finding(index: usize, severity: Severity) -> Result<ValidFinding, Box<dyn Error>> {
    let line = NonZeroU64::new(12).ok_or("12 is not zero")?;
    Ok(ValidFinding {
        at: FindingRef::Finding { document: 0, index },
        finding: Finding {
            class: Class::ErrorHandling,
            rule: Some("H-02".to_owned()),
            kind: FindingKind::Defect,
            severity,
            introduced_by: None,
            pre_existing: false,
            file: "src/old_name.rs".to_owned(),
            line,
            end_line: None,
            side: None,
            evidence: "let x = input.parse().unwrap();".to_owned(),
            expected_behavior: None,
            trigger: Some("a non-numeric input reaches line 12 through parse".to_owned()),
            caller: None,
            title: format!("unwrap on user input panics ({index})"),
            body: "RATIONALE the verifier must not see".to_owned(),
            fix: Some("FIX the verifier must not see".to_owned()),
            confidence: Some(Confidence::High),
            group_hint: Some("GROUP the verifier must not see".to_owned()),
        },
        file_at_head: "src/lib.rs".to_owned(),
        enclosing_symbol: "parse".to_owned(),
        occurrence_index: 0,
        fingerprint: "0123456789abcdef".parse()?,
        placement: Placement::Inline,
        previously_dismissed: None,
    })
}

/// [`finding`] with `confidence`.
pub(crate) fn finding_sure(
    index: usize,
    severity: Severity,
    confidence: Option<Confidence>,
) -> Result<ValidFinding, Box<dyn Error>> {
    let mut found = finding(index, severity)?;
    found.finding.confidence = confidence;
    Ok(found)
}

/// A finding of `severity` open from an earlier run, titled `carried <index>`, now at line 40 of
/// `src/moved.rs`.
pub(crate) fn carried(index: usize, severity: Severity) -> Result<CarriedFinding, Box<dyn Error>> {
    let earlier = finding(index, severity)?;
    Ok(CarriedFinding::read(
        CarriedParts {
            fp: format!("{index:016x}").parse()?,
            file: "src/moved.rs".to_owned(),
            line: NonZeroU64::new(40).ok_or("40 is not zero")?,
            class: earlier.finding.class,
            severity,
            title: format!("carried {index}"),
            trigger: Some("an empty list reaches the index".to_owned()),
            evidence: earlier.finding.evidence,
        },
        REPOSITORY,
    )?)
}

/// The `claim` of each call `runner` was given, in order.
pub(crate) fn claims(runner: &Scripted) -> Vec<String> {
    runner
        .calls
        .borrow()
        .iter()
        .map(|call| {
            call.user_prompt
                .as_str()
                .split_once("\n\n")
                .and_then(|(claim, _)| serde_json::from_str::<Value>(claim).ok())
                .and_then(|claim| claim["claim"].as_str().map(str::to_owned))
                .unwrap_or_default()
        })
        .collect()
}
