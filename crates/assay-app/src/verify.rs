//! The blind verifier (`docs/architecture/04-pipeline.md` §3, §4 step 8): one model call per
//! candidate finding that tries to refute it, seeing only `{file, line, class, claim, trigger}`.
//!
//! It runs after Z3's mechanical validation, so of this run's findings it only ever judges
//! [`ValidFinding`]s; findings carried from earlier runs ([`CarriedFinding`]) share its calls
//! and go first (04 §7). The caller gives it its own [`ModelRunner`], configured on a different
//! model tier from the specialists' (04 §3, `A-14`): the port's calls carry no model of their
//! own.

mod candidate;
mod outcome;
mod tally;
mod tier;
mod verdict;

use std::num::NonZeroU8;
use std::path::PathBuf;
use std::time::Duration;

use assay_domain::{Aeu, CallFailure, Severity};

pub use candidate::{CarriedFinding, ClaimRef};
pub use outcome::{Verification, VerifiedFinding, VerifierFailure, VerifierStatus};
pub use tally::{REJECT_RATE_MIN_RUNS, RejectRate, VerdictTally};
pub use tier::{UnsupportedTier, VerifierTier};
pub use verdict::{Judgement, MAX_REASON_CHARS, VERDICT_SCHEMA, VerdictReason, VerdictRefusal};

use self::candidate::{Candidate, Slot, candidates, is_candidate};
use crate::model_runner::{ModelCall, ModelOutcome, ModelRunner};
use crate::prompt_bundle::PromptBundle;
use crate::validation::ValidFinding;

/// The verifier's turn limit per call (04 §3 roster).
pub const VERIFIER_MAX_TURNS: NonZeroU8 = match NonZeroU8::new(8) {
    Some(turns) => turns,
    None => NonZeroU8::MIN,
};

/// The verifier's cap per call (04 §3 roster), provisional until calibration (`A-21`).
pub const VERIFIER_CAP: Aeu = Aeu::from_micros(80_000);

/// Attempts per candidate: the first, and one retry of malformed output (03 §7).
const ATTEMPTS: u8 = 2;

/// What to verify, and how to call the model.
#[derive(Debug)]
pub struct VerifyRequest<'a> {
    /// The PR's tier, which caps the calls.
    pub tier: VerifierTier,
    /// The repository under review, `owner/name`, for the injection screen of the reasons.
    pub repository: &'a str,
    /// The findings Z3's validation kept, in its order.
    pub findings: &'a [ValidFinding],
    /// The findings open from earlier runs of the PR, to judge again at head (04 §7); empty
    /// until re-review (#99) supplies them.
    pub carried: &'a [CarriedFinding],
    /// The prompt bundle, whose verifier instruction follows each claim.
    pub bundle: &'a PromptBundle,
    /// The base-sourced system prompt file (absolute path), as for every role.
    pub system_prompt_file: PathBuf,
    /// The read-only PR tree the verifier may read (absolute path).
    pub add_dir: PathBuf,
    /// The wall-clock limit of each call.
    pub timeout: Duration,
}

/// Verifies the carried findings and the candidates among `request.findings` through `runner`
/// (04 §4 step 8, §7).
///
/// Every carried finding is a candidate, and goes first, by severity then in order; then the
/// raised findings of at least `major`, blockers first, then by confidence, then in order. At
/// most five are judged in `full` and three in `lite`; `micro` makes no call, and a candidate
/// past the cap keeps its severity, unverified. Each call sees one claim and the verifier
/// instruction, with an 8-turn limit and a $0.08 cap, and the stage as a whole spends at most
/// $0.08 per call ([`VerifierTier::budget`]): an attempt gets only what is left, and none left
/// stops the stage as `cap`. Malformed output, or a verdict outside the exact shape, is retried
/// once and is otherwise `malformed`. The first call that ends without a verdict stops the
/// stage: findings judged so far keep their verdicts and the candidates not yet judged are
/// unverified, shown as a major at most. Total: every failure is an outcome in the returned
/// [`Verification`].
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use std::convert::Infallible;
/// use std::path::PathBuf;
/// use std::time::Duration;
///
/// use assay_app::{
///     BundleFiles, Change, ContractValidator, EgressScan, InitReadError, Inputs, KnownRuns,
///     ModelCall, ModelOutcome, ModelResult, ModelRunner, PromptBundle, Structured,
///     SuppressRules, VerifierTier, VerifyRequest, validate, verify,
/// };
/// use assay_domain::{Aeu, Finding, Severity, SupportOutcome, VerifierVerdict};
/// use serde_json::json;
///
/// struct NoRules;
///
/// impl SuppressRules for NoRules {
///     fn suppresses(&self, _finding: &Finding, _file_at_head: &str) -> bool {
///         false
///     }
/// }
///
/// /// A model that refutes every claim.
/// struct Refuting;
///
/// impl ModelRunner for Refuting {
///     type Error = Infallible;
///
///     fn run(&self, _call: &ModelCall) -> Result<ModelResult, Infallible> {
///         let verdict = json!({"verdict": "refuted", "reason": "only digits reach parse"});
///         Ok(ModelResult {
///             outcome: ModelOutcome::Ok(Structured::new(verdict)),
///             cost: Aeu::from_micros(20_000),
///             rate_limits: Vec::new(),
///             init: Err(InitReadError::Missing),
///         })
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let lib = "fn f() {\n    parse(x).unwrap();\n}\n";
/// let change = Change {
///     head_files: BTreeMap::from([("src/lib.rs".to_owned(), lib.to_owned())]),
///     ..Change::default()
/// };
/// let egress = EgressScan::without_token(KnownRuns::index([lib.as_bytes()])?);
/// let inputs = Inputs {
///     repository: "owner/repo",
///     change: &change,
///     suppress: &NoRules,
///     dismissals: &[],
///     gate_findings: &[],
///     egress: &egress,
/// };
/// let document = br#"{"schema": "assay.findings/1",
///     "source": {"kind": "reviewer", "name": "claude", "version": "v", "role": "combined",
///                "run_key": "r"},
///     "findings": [{"class": "logic", "kind": "defect", "severity": "major", "file": "src/lib.rs",
///                   "line": 2, "evidence": "parse(x).unwrap();", "trigger": "a non-numeric x",
///                   "title": "unwrap panics", "body": "b"}]}"#;
/// let validated = validate(&ContractValidator::new()?, &inputs, &[document]);
/// let bundle = PromptBundle::new(BundleFiles {
///     system: "system".to_owned(),
///     bugs: "bugs".to_owned(),
///     security: "security".to_owned(),
///     design: "design".to_owned(),
///     meta: "meta".to_owned(),
///     combined: "combined".to_owned(),
///     combined_micro: "micro".to_owned(),
///     verifier: "verifier".to_owned(),
/// })?;
///
/// let verified = verify(&Refuting, VerifyRequest {
///     tier: VerifierTier::Lite,
///     repository: "owner/repo",
///     findings: &validated.findings,
///     carried: &[],
///     bundle: &bundle,
///     system_prompt_file: PathBuf::from("/base/system.md"),
///     add_dir: PathBuf::from("/work/pr"),
///     timeout: Duration::from_secs(300),
/// });
///
/// // Refuted: downgraded to minor, and the verifier completed.
/// let found = verified.findings.first().ok_or("the major was verified")?;
/// let refuted = Some(VerifierVerdict::Refuted);
/// assert_eq!((found.severity, found.verdict()), (Severity::Minor, refuted));
/// assert_eq!(verified.outcome(), SupportOutcome::Completed);
/// # Ok(())
/// # }
/// ```
pub fn verify<R: ModelRunner>(runner: &R, request: VerifyRequest<'_>) -> Verification<R::Error> {
    let VerifyRequest {
        tier,
        repository,
        findings,
        carried,
        bundle,
        system_prompt_file,
        add_dir,
        timeout,
    } = request;
    let mut verification = unjudged_all(tier, findings, carried);
    let picked = candidates(tier, carried, findings);
    let Some(first) = picked.first() else {
        return verification;
    };
    let instruction = bundle.verifier_instruction();
    // One call, its prompt and budget replaced per attempt: the paths move in once.
    let mut call = match first.prompt(instruction) {
        Ok(user_prompt) => ModelCall {
            system_prompt_file,
            user_prompt,
            json_schema: VERDICT_SCHEMA.to_owned(),
            add_dir,
            max_turns: VERIFIER_MAX_TURNS,
            budget: VERIFIER_CAP,
            timeout,
        },
        Err(_) => return verification.stopped(unpromptable(first.at), slots(&picked)),
    };
    let stage = Stage {
        repository,
        budget: tier.budget(),
    };
    for (position, candidate) in picked.iter().enumerate() {
        let unreached = || slots(&picked).skip(position);
        if position > 0 {
            match candidate.prompt(instruction) {
                Ok(user_prompt) => call.user_prompt = user_prompt,
                Err(_) => return verification.stopped(unpromptable(candidate.at), unreached()),
            }
        }
        match stage.judge(runner, &mut call, &mut verification) {
            Ok(judgement) => verification.record(candidate.slot, judgement),
            Err(ended) => return verification.stopped(ended.at(candidate.at), unreached()),
        }
    }
    verification
}

/// The stage before any call: every finding [`unjudged`], nothing spent.
fn unjudged_all<E>(
    tier: VerifierTier,
    findings: &[ValidFinding],
    carried: &[CarriedFinding],
) -> Verification<E> {
    Verification {
        findings: findings
            .iter()
            .map(|found| unjudged(ClaimRef::Raised(found.at), found.finding.severity, tier))
            .collect(),
        carried: carried
            .iter()
            .enumerate()
            .map(|(index, found)| {
                unjudged(ClaimRef::Carried { index }, found.finding.severity, tier)
            })
            .collect(),
        failure: None,
        attempts: 0,
        cost: Aeu::ZERO,
        rate_limits: Vec::new(),
        refusals: Vec::new(),
    }
}

/// A finding before any call: a raised finding below `major` is not a candidate, nothing is in
/// `micro`, and any other is a candidate past the cap until it is judged.
fn unjudged(at: ClaimRef, raised: Severity, tier: VerifierTier) -> VerifiedFinding {
    let status = match at {
        ClaimRef::Raised(_) if !is_candidate(raised) => VerifierStatus::NotCandidate,
        ClaimRef::Raised(_) | ClaimRef::Carried { .. } if tier == VerifierTier::Micro => {
            VerifierStatus::NoVerifier
        }
        ClaimRef::Raised(_) | ClaimRef::Carried { .. } => VerifierStatus::OverCap,
    };
    VerifiedFinding {
        at,
        raised,
        severity: raised,
        status,
    }
}

/// Where each of `candidates` is recorded.
fn slots<'a>(candidates: &'a [Candidate<'_>]) -> impl Iterator<Item = Slot> + 'a {
    candidates.iter().map(|candidate| candidate.slot)
}

/// The failure of a claim that is not a valid prompt: no model could be asked. A JSON object
/// starts with `{` and escapes NUL, and the bundle refuses NUL, so this does not happen.
const fn unpromptable<E>(at: ClaimRef) -> VerifierFailure<E> {
    VerifierFailure::Call {
        at,
        reason: CallFailure::Unavailable,
    }
}

/// How a candidate's call ended without a verdict, before the candidate is attached.
enum Ended<E> {
    Call(CallFailure),
    Runner(E),
}

impl<E> Ended<E> {
    fn at(self, at: ClaimRef) -> VerifierFailure<E> {
        match self {
            Self::Call(reason) => VerifierFailure::Call { at, reason },
            Self::Runner(source) => VerifierFailure::Runner { at, source },
        }
    }
}

/// What every call of the stage shares: the repository for the reasons' injection screen and
/// the stage budget.
struct Stage<'a> {
    repository: &'a str,
    budget: Aeu,
}

impl Stage<'_> {
    /// Makes `call` until it yields a verdict, at most [`ATTEMPTS`] times, each attempt with the
    /// smaller of [`VERIFIER_CAP`] and what is left of the stage budget, charging every attempt
    /// to `spent`. Only malformed output and a refused verdict are retried; nothing left is
    /// `cap`, with no call.
    fn judge<R: ModelRunner>(
        &self,
        runner: &R,
        call: &mut ModelCall,
        spent: &mut Verification<R::Error>,
    ) -> Result<Judgement, Ended<R::Error>> {
        for _ in 0..ATTEMPTS {
            let left = Aeu::from_micros(self.budget.micros().saturating_sub(spent.cost.micros()));
            if left == Aeu::ZERO {
                return Err(Ended::Call(CallFailure::Cap));
            }
            call.budget = left.min(VERIFIER_CAP);
            spent.attempts = spent.attempts.saturating_add(1);
            let result = match runner.run(call) {
                Ok(result) => result,
                Err(source) => {
                    // The model may have started; its cost is unknown, so its budget is charged.
                    spent.cost = spent.cost.saturating_add(call.budget);
                    return Err(Ended::Runner(source));
                }
            };
            spent.cost = spent.cost.saturating_add(result.cost);
            spent.rate_limits.extend(result.rate_limits);
            let failure = match result.outcome {
                ModelOutcome::Ok(structured) => {
                    match verdict::read(structured.into_value(), self.repository) {
                        Ok(judgement) => return Ok(judgement),
                        Err(refusal) => {
                            spent.refusals.push(refusal);
                            continue;
                        }
                    }
                }
                ModelOutcome::Malformed => continue,
                ModelOutcome::BudgetExhausted => CallFailure::Cap,
                ModelOutcome::TurnsExhausted => CallFailure::Turns,
                ModelOutcome::Timeout => CallFailure::Timeout,
                ModelOutcome::QuotaExhausted => CallFailure::Unavailable,
            };
            return Err(Ended::Call(failure));
        }
        Err(Ended::Call(CallFailure::Malformed))
    }
}
