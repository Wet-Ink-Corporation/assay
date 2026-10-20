//! The coordinator (S5, `docs/architecture/04-pipeline.md` §3, §4 step 7): one model call that
//! groups the run's validated findings by root cause, merges duplicates and marks noise, with a
//! deterministic merge that stands in whenever the call fails or its answer is refused.
//!
//! It runs after Z3's validation and before the verifier, so it only ever sees
//! [`ValidFinding`]s, and only through ids: its answer names findings, it never writes, rewords
//! or re-rates one, and nothing it returns is published as text. Every merge obeys one rule
//! (a duplicate cites the same file and side, is no more severe and hides no new finding behind
//! a pre-existing one); a merge the coordinator asks for must also be of a `minor` or `nit` of
//! the keeper's class and kind on overlapping lines, so only a fingerprint merge absorbs a
//! major. Every group shares a `class` and holds at most one major, so the coordinator never
//! lowers rule 7's count, and noise only moves a `minor` or `nit` to the summary. The ranking is mechanical: severity, then confidence, then the
//! coordinator's order.

mod answer;
mod merge;
mod outcome;

#[cfg(test)]
mod fixture;

use std::collections::BTreeMap;
use std::num::NonZeroU8;
use std::path::PathBuf;
use std::time::Duration;

use assay_domain::{Aeu, CallFailure, Confidence};
use serde_json::{Value, json};

pub use answer::{COORDINATION_SCHEMA, CoordinationRefusal};
pub use outcome::{Coordination, CoordinatorFailure, GroupId, Merged, MergedBy, Ranked};

use self::answer::{Plan, Shown};
use self::merge::{Distinct, Entry, distinct, mechanical, rank};
use crate::model_runner::{
    InvalidPrompt, ModelCall, ModelOutcome, ModelRunner, Prompt, RateLimitObserved,
};
use crate::prompt_bundle::PromptBundle;
use crate::validation::ValidFinding;

/// The coordinator's turn limit per call (04 §3 roster).
pub const COORDINATOR_MAX_TURNS: NonZeroU8 = match NonZeroU8::new(10) {
    Some(turns) => turns,
    None => NonZeroU8::MIN,
};

/// The coordinator's cap (04 §3 roster): what the stage spends at most, its retry included.
/// Provisional until calibration (`A-21`).
pub const COORDINATOR_CAP: Aeu = Aeu::from_micros(300_000);

/// The most distinct findings one coordinator call is shown. A run with more is merged
/// mechanically without a call, and the coordinator is degraded as `cap` (H-13).
pub const MAX_COORDINATED_FINDINGS: usize = 64;

/// Attempts: the first, and one retry of malformed or refused output (03 §7).
const ATTEMPTS: u8 = 2;

/// What to coordinate, and how to call the model.
#[derive(Debug)]
pub struct CoordinateRequest<'a> {
    /// The findings Z3's validation kept, in its order.
    pub findings: &'a [ValidFinding],
    /// The prompt bundle, whose coordinator instruction follows the findings.
    pub bundle: &'a PromptBundle,
    /// The base-sourced system prompt file (absolute path), as for every role.
    pub system_prompt_file: PathBuf,
    /// The read-only PR tree the coordinator may read (absolute path).
    pub add_dir: PathBuf,
    /// The wall-clock limit of each call.
    pub timeout: Duration,
}

/// Coordinates `request.findings` on the `full` tier through `runner` (04 §4 step 7).
///
/// Findings with the same fingerprint are merged first, mechanically. With none left there is
/// no call. With more than [`MAX_COORDINATED_FINDINGS`] left there is no call either, and the
/// coordinator fails as `cap`. Otherwise the coordinator is shown each finding's id, `class`,
/// `severity`, `confidence`, `pre_existing`, `file`, `line`, `title`, `evidence`, `trigger`
/// and `group_hint`, and answers with groups, duplicates and noise in ids only. The answer is
/// read strictly ([`CoordinationRefusal`]); a refused or malformed answer is retried once. The
/// stage spends at most [`COORDINATOR_CAP`], each attempt getting what is left, with a 10-turn
/// limit. When no answer is accepted, the findings are merged mechanically
/// ([`merge_mechanically`]) and the failure is kept, so `decide` sees the coordinator degraded.
/// Total: every failure is an outcome in the returned [`Coordination`].
///
/// # Examples
///
/// ```
/// use std::convert::Infallible;
/// use std::num::NonZeroU64;
/// use std::path::PathBuf;
/// use std::time::Duration;
///
/// use assay_app::{
///     BundleFiles, CoordinateRequest, FindingRef, InitReadError, MergedBy, ModelCall,
///     ModelOutcome, ModelResult, ModelRunner, Placement, PromptBundle, Structured,
///     ValidFinding, coordinate,
/// };
/// use assay_domain::{Aeu, Class, Finding, FindingKind, Severity, SupportOutcome};
/// use serde_json::json;
///
/// /// A coordinator that puts both findings in one group, the minor first.
/// struct OneGroup;
///
/// impl ModelRunner for OneGroup {
///     type Error = Infallible;
///
///     fn run(&self, _call: &ModelCall) -> Result<ModelResult, Infallible> {
///         let answer = json!({"groups": [{"findings": [1, 0], "duplicates": []}], "noise": []});
///         Ok(ModelResult {
///             outcome: ModelOutcome::Ok(Structured::new(answer)),
///             cost: Aeu::from_micros(50_000),
///             rate_limits: Vec::new(),
///             init: Err(InitReadError::Missing),
///         })
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let found = |index: usize, line: u64, fp: &str, severity| -> Result<ValidFinding, Box<dyn std::error::Error>> {
///     Ok(ValidFinding {
///         at: FindingRef::Finding { document: 0, index },
///         finding: Finding {
///             class: Class::Logic, rule: None, kind: FindingKind::Defect,
///             severity, introduced_by: None, pre_existing: false,
///             file: "src/lib.rs".to_owned(), line: NonZeroU64::new(line).ok_or("zero")?,
///             end_line: None, side: None, evidence: "check(x);".to_owned(),
///             expected_behavior: None, trigger: Some("an empty x".to_owned()), caller: None,
///             title: "unchecked".to_owned(), body: "b".to_owned(), fix: None,
///             confidence: None, group_hint: None,
///         },
///         file_at_head: "src/lib.rs".to_owned(),
///         enclosing_symbol: "f".to_owned(),
///         occurrence_index: 0,
///         fingerprint: fp.parse()?,
///         placement: Placement::Inline,
///         previously_dismissed: None,
///     })
/// };
/// let findings = [
///     found(0, 3, "0000000000000001", Severity::Major)?,
///     found(1, 9, "0000000000000002", Severity::Minor)?,
/// ];
/// let text = |s: &str| s.to_owned();
/// let bundle = PromptBundle::new(BundleFiles {
///     system: text("system"), bugs: text("bugs"), security: text("security"),
///     design: text("design"), meta: text("meta"), combined: text("combined"),
///     combined_micro: text("micro"), verifier: text("verifier"),
///     verifier_carried: text("verifier-carried"), coordinator: text("coordinator"),
/// })?;
///
/// let coordinated = coordinate(&OneGroup, CoordinateRequest {
///     findings: &findings,
///     bundle: &bundle,
///     system_prompt_file: PathBuf::from("/base/system.md"),
///     add_dir: PathBuf::from("/work/pr"),
///     timeout: Duration::from_secs(300),
/// });
///
/// // One group; the major ranks first whatever the coordinator's order, and it completed.
/// let order: Vec<_> = coordinated.ranked.iter().map(|r| (r.index, r.group.to_string())).collect();
/// assert_eq!(order, [(0, "g1".to_owned()), (1, "g1".to_owned())]);
/// assert_eq!((coordinated.merged_by, coordinated.outcome()), (MergedBy::Coordinator, SupportOutcome::Completed));
/// # Ok(())
/// # }
/// ```
pub fn coordinate<R: ModelRunner>(
    runner: &R,
    request: CoordinateRequest<'_>,
) -> Coordination<R::Error> {
    let CoordinateRequest {
        findings,
        bundle,
        system_prompt_file,
        add_dir,
        timeout,
    } = request;
    let distinct = distinct(findings);
    if distinct.kept.is_empty() {
        return merge_mechanically(findings);
    }
    if distinct.kept.len() > MAX_COORDINATED_FINDINGS {
        return merge_mechanically(findings).failed(CoordinatorFailure::TooMany {
            findings: distinct.kept.len(),
        });
    }
    let shown: Vec<Shown<'_>> = distinct
        .kept
        .iter()
        .filter_map(|&index| {
            Some(Shown {
                index,
                found: findings.get(index)?,
            })
        })
        .collect();
    let Ok(user_prompt) = prompt(&shown, bundle.coordinator_instruction()) else {
        return merge_mechanically(findings).failed(unpromptable());
    };
    let mut call = ModelCall {
        system_prompt_file,
        user_prompt,
        json_schema: COORDINATION_SCHEMA.to_owned(),
        add_dir,
        max_turns: COORDINATOR_MAX_TURNS,
        budget: COORDINATOR_CAP,
        timeout,
    };
    let mut spent = Spent::default();
    match ask(runner, &mut call, &shown, &mut spent) {
        Ok(plan) => spent.charge(coordinated(findings, distinct, plan)),
        Err(failure) => spent.charge(merge_mechanically(findings).failed(failure)),
    }
}

/// Merges `findings` without a model, for the `lite` and `micro` tiers, which have no
/// coordinator; its outcome is `NotNeeded`.
///
/// Findings with the same fingerprint are merged into the one that ranks highest when the
/// merge hides nothing, every other finding is a group of its own, and the rank is severity,
/// then confidence, then input order.
#[must_use]
pub fn merge_mechanically<E>(findings: &[ValidFinding]) -> Coordination<E> {
    let (ranked, merged) = mechanical(findings);
    Coordination {
        ranked,
        merged,
        merged_by: MergedBy::Mechanical,
        failure: None,
        attempts: 0,
        cost: Aeu::ZERO,
        rate_limits: Vec::new(),
        refusals: Vec::new(),
    }
}

/// The coordination of an accepted `plan`, whose positions `read` already mapped from ids, with
/// the merges of `distinct` made before the call.
fn coordinated<E>(findings: &[ValidFinding], distinct: Distinct, plan: Plan) -> Coordination<E> {
    let Distinct { mut merged, .. } = distinct;
    let Plan { groups, duplicates } = plan;
    let entries = groups
        .into_iter()
        .enumerate()
        .flat_map(|(group, members)| {
            members.into_iter().map(move |member| Entry {
                index: member.index,
                group,
                noise: member.noise,
            })
        })
        .collect();
    let by_coordinator: BTreeMap<usize, Merged> = duplicates
        .into_iter()
        .map(|merge| (merge.index, merge))
        .collect();
    // A finding merged before the call follows its keeper when the coordinator merged that
    // keeper too, so every merge names a kept finding; the coordinator's keepers are kept.
    for merge in &mut merged {
        if let Some(keeper) = by_coordinator.get(&merge.into) {
            merge.into = keeper.into;
        }
    }
    merged.extend(by_coordinator.into_values());
    merged.sort_by_key(|merge| merge.index);
    Coordination {
        ranked: rank(findings, entries),
        merged,
        merged_by: MergedBy::Coordinator,
        failure: None,
        attempts: 0,
        cost: Aeu::ZERO,
        rate_limits: Vec::new(),
        refusals: Vec::new(),
    }
}

/// The coordinator's user prompt: the findings it may see as one JSON object, a blank line,
/// then the instruction. Only the listed fields are shown; never `body`, `fix` or the rest.
fn prompt(shown: &[Shown<'_>], instruction: &str) -> Result<Prompt, InvalidPrompt> {
    let findings: Vec<Value> = shown
        .iter()
        .enumerate()
        .map(|(id, shown)| {
            let found = shown.found;
            let finding = &found.finding;
            json!({
                "id": id,
                "class": finding.class.code(),
                "severity": finding.severity.code(),
                "confidence": finding.confidence.map(Confidence::code),
                "pre_existing": finding.pre_existing,
                "file": found.file_at_head,
                "line": finding.line,
                "title": finding.title,
                "evidence": finding.evidence,
                "trigger": finding.trigger,
                "group_hint": finding.group_hint,
            })
        })
        .collect();
    let shown = json!({ "findings": findings });
    Prompt::new(format!("{shown}\n\n{instruction}"))
}

/// The failure of findings that are not a valid prompt: no model could be asked. A JSON object
/// starts with `{` and escapes NUL, and the bundle refuses NUL, so this does not happen.
const fn unpromptable<E>() -> CoordinatorFailure<E> {
    CoordinatorFailure::Call(CallFailure::Unavailable)
}

/// What the stage's attempts spent.
#[derive(Debug, Default)]
struct Spent {
    attempts: u32,
    cost: Aeu,
    rate_limits: Vec<RateLimitObserved>,
    refusals: Vec<CoordinationRefusal>,
}

impl Spent {
    /// `coordination` charged with what was spent.
    fn charge<E>(self, coordination: Coordination<E>) -> Coordination<E> {
        Coordination {
            attempts: self.attempts,
            cost: self.cost,
            rate_limits: self.rate_limits,
            refusals: self.refusals,
            ..coordination
        }
    }
}

impl<E> Coordination<E> {
    /// This coordination with the coordinator failed on `failure`.
    fn failed(self, failure: CoordinatorFailure<E>) -> Self {
        Self {
            failure: Some(failure),
            ..self
        }
    }
}

/// Makes `call` until an answer is accepted, at most [`ATTEMPTS`] times, each attempt with what
/// is left of [`COORDINATOR_CAP`], charging every attempt to `spent`. Only malformed output and
/// a refused answer are retried; nothing left is `cap`, with no call.
fn ask<R: ModelRunner>(
    runner: &R,
    call: &mut ModelCall,
    shown: &[Shown<'_>],
    spent: &mut Spent,
) -> Result<Plan, CoordinatorFailure<R::Error>> {
    for _ in 0..ATTEMPTS {
        let left = Aeu::from_micros(COORDINATOR_CAP.micros().saturating_sub(spent.cost.micros()));
        if left == Aeu::ZERO {
            return Err(CoordinatorFailure::Call(CallFailure::Cap));
        }
        call.budget = left;
        spent.attempts = spent.attempts.saturating_add(1);
        let result = match runner.run(call) {
            Ok(result) => result,
            Err(source) => {
                // The model may have started; its cost is unknown, so its budget is charged.
                spent.cost = spent.cost.saturating_add(call.budget);
                return Err(CoordinatorFailure::Runner(source));
            }
        };
        spent.cost = spent.cost.saturating_add(result.cost);
        spent.rate_limits.extend(result.rate_limits);
        let failure = match result.outcome {
            ModelOutcome::Ok(structured) => match answer::read(structured.into_value(), shown) {
                Ok(plan) => return Ok(plan),
                Err(refusal) => {
                    spent.refusals.push(refusal);
                    continue;
                }
            },
            ModelOutcome::Malformed => continue,
            ModelOutcome::BudgetExhausted => CallFailure::Cap,
            ModelOutcome::TurnsExhausted => CallFailure::Turns,
            ModelOutcome::Timeout => CallFailure::Timeout,
            ModelOutcome::QuotaExhausted => CallFailure::Unavailable,
        };
        return Err(CoordinatorFailure::Call(failure));
    }
    Err(CoordinatorFailure::Call(CallFailure::Malformed))
}
