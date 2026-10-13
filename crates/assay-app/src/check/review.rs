//! The local review: one model call with the tier's role, its output stamped with assay's own
//! `source` and validated as `decide` validates a review job's documents (04 §4).
//!
//! The v1 bundle has two instructions, so `micro` is reviewed by `combined-micro` and `lite` and
//! `full` by `combined`; a `full` PR's specialist roster is the review job's (#85), and the
//! decision reports those roles missing.

use assay_domain::{
    Aeu, CallFailure, Disposition, Finding, FindingsSchema, RequiredRole, SourceKind, Tier,
};
use serde_json::{Map, Value, json};

use super::ReviewSetup;
use crate::contract::{Contract, ContractValidator};
use crate::model_runner::{
    ModelAttempts, ModelCall, ModelOutcome, ModelResult, ModelRunner, RetryError, run_with_retry,
};
use crate::plan::EvidenceKey;
use crate::prompt_bundle::Role;
use crate::session_init::{CLEAN_TOOLS, InitReadError, SessionInit};
use crate::validation::{
    Change, EgressScan, FindingRef, Inputs, SuppressRules, ValidFinding, validate,
};

/// The reviewer name local mode stamps on its document.
const REVIEWER: &str = "assay-local";

/// A role local mode can run: its one call is a combined pass, so the `full` specialists
/// (`bugs`, `security`, `design`, `meta`) cannot be named here, and their absence is what the
/// decision reports as missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LocalRole {
    /// `combined`, for `lite` and `full`.
    Combined,
    /// `combined-micro`, for `micro`.
    CombinedMicro,
}

impl LocalRole {
    /// The bundle's role: its instruction, limits and code.
    pub(super) const fn bundle(self) -> Role {
        match self {
            Self::Combined => Role::Combined,
            Self::CombinedMicro => Role::CombinedMicro,
        }
    }

    /// The roster role a completed call reports as.
    pub(super) const fn required(self) -> RequiredRole {
        match self {
            Self::Combined => RequiredRole::Combined,
            Self::CombinedMicro => RequiredRole::CombinedMicro,
        }
    }
}

/// The role that reviews `tier`; `None` for a tier without an LLM stage.
pub(super) const fn role(tier: Tier) -> Option<LocalRole> {
    match tier {
        Tier::Micro => Some(LocalRole::CombinedMicro),
        Tier::Lite | Tier::Full => Some(LocalRole::Combined),
        Tier::Trivial | Tier::NeedsSplit => None,
    }
}

/// No base `suppress` rules: local mode has no parser for them yet, so it shows every finding.
struct NoSuppression;

impl SuppressRules for NoSuppression {
    fn suppresses(&self, _finding: &Finding, _file_at_head: &str) -> bool {
        false
    }
}

/// What the review's document is validated against: the change, and the gate findings, as in
/// Z4. There are no owner dismissals locally.
pub(super) fn inputs<'a>(
    repository: &'a str,
    change: &'a Change,
    gate_findings: &'a [Finding],
    egress: &'a EgressScan<'a>,
) -> Inputs<'a> {
    Inputs {
        repository,
        change,
        suppress: &NoSuppression,
        dismissals: &[],
        gate_findings,
        egress,
    }
}

/// How the review ended.
#[derive(Debug)]
pub(super) enum Stage {
    /// The model returned structured output; these findings survived validation.
    Completed(Vec<ValidFinding>),
    /// The call ran and produced nothing usable.
    Degraded(CallFailure),
    /// No usable call could be made.
    Unavailable,
}

/// The review's outcome, with what the report says about it.
#[derive(Debug)]
pub(super) struct Reviewed {
    pub(super) stage: Stage,
    /// Why it did not complete, or what validation refused, in assay's words.
    pub(super) note: Option<String>,
    /// Every attempt's cost; a call whose cost is unknown is charged its cap.
    pub(super) cost: Aeu,
}

impl Reviewed {
    fn unavailable(note: String, cost: Aeu) -> Self {
        Self {
            stage: Stage::Unavailable,
            note: Some(note),
            cost,
        }
    }

    fn degraded(failure: CallFailure, cost: Aeu) -> Self {
        Self {
            stage: Stage::Degraded(failure),
            note: Some(format!("the review degraded: {}", failure.code())),
            cost,
        }
    }
}

/// Makes the review's call.
pub(super) struct Reviewer<'a, M> {
    pub(super) setup: &'a ReviewSetup<'a>,
    pub(super) model: &'a M,
    pub(super) validator: &'a ContractValidator,
    pub(super) run_key: EvidenceKey,
}

impl<M: ModelRunner> Reviewer<'_, M> {
    /// Reviews the change `pack` describes as `role`, validating the output against `inputs`.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the stamped document cannot be encoded.
    pub(super) fn review(
        &self,
        local: LocalRole,
        pack: &str,
        inputs: &Inputs<'_>,
    ) -> Result<Reviewed, serde_json::Error> {
        let role = local.bundle();
        let Some(call) = self.call(role, pack) else {
            let note = "the context pack is not a valid prompt".to_owned();
            return Ok(Reviewed::unavailable(note, Aeu::ZERO));
        };
        let attempts = match run_with_retry(self.model, &call) {
            Ok(attempts) => attempts,
            Err(RetryError::First(error)) => {
                return Ok(Reviewed::unavailable(error.to_string(), role.cap()));
            }
            Err(RetryError::Retry { first, source }) => {
                let cost = first.cost.saturating_add(role.cap());
                return Ok(Reviewed::unavailable(source.to_string(), cost));
            }
        };
        let cost = attempts.cost();
        let ModelAttempts { first, retry } = attempts;
        let ModelResult { outcome, init, .. } = retry.unwrap_or(first);
        if let Err(note) = confined(&init) {
            return Ok(Reviewed::unavailable(note, cost));
        }
        let structured = match outcome {
            ModelOutcome::Ok(structured) => structured,
            ModelOutcome::Malformed => return Ok(Reviewed::degraded(CallFailure::Malformed, cost)),
            ModelOutcome::BudgetExhausted => return Ok(Reviewed::degraded(CallFailure::Cap, cost)),
            ModelOutcome::TurnsExhausted => {
                return Ok(Reviewed::degraded(CallFailure::Turns, cost));
            }
            ModelOutcome::Timeout => return Ok(Reviewed::degraded(CallFailure::Timeout, cost)),
            ModelOutcome::QuotaExhausted => {
                return Ok(Reviewed::degraded(CallFailure::Unavailable, cost));
            }
        };
        let document = serde_json::to_vec(&self.stamp(structured.into_value(), role))?;
        let validated = validate(self.validator, inputs, &[document.as_slice()]);
        if validated.raises_egress_alarm() {
            // As the CI review job fails on an alarm, none of this output is shown.
            let note = format!(
                "the egress scan found {} piece(s) of model text the snapshot does not explain; \
                 the review's findings are withheld",
                validated.egress.len()
            );
            return Ok(Reviewed::unavailable(note, cost));
        }
        let refused = validated.transitions.iter().any(|transition| {
            matches!(transition.at, FindingRef::Document { .. })
                && transition.code == Disposition::Schema
        });
        Ok(Reviewed {
            stage: Stage::Completed(validated.findings),
            note: refused.then(|| "the model's document failed the findings schema".to_owned()),
            cost,
        })
    }

    /// The call that reviews `pack` as `role`; `None` when the pack is not a valid prompt.
    fn call(&self, role: Role, pack: &str) -> Option<ModelCall> {
        let user_prompt = self.setup.bundle.user_prompt(role, pack).ok()?;
        Some(ModelCall {
            system_prompt_file: self.setup.system_prompt_file.to_path_buf(),
            user_prompt,
            json_schema: Contract::Findings.schema_text().to_owned(),
            add_dir: self.setup.tree_dir.to_path_buf(),
            max_turns: role.max_turns(),
            budget: role.cap(),
            timeout: self.setup.timeout,
        })
    }

    /// `output` as an `assay.findings/1` document whose `schema` and `source` are assay's: a
    /// model-written `source` is replaced, so validation holds the findings to `role`'s
    /// ceiling. Anything but an object is left alone, and validation refuses it.
    fn stamp(&self, output: Value, role: Role) -> Value {
        let mut document: Map<String, Value> = match output {
            Value::Object(document) => document,
            other => return other,
        };
        document.insert("schema".to_owned(), json!(FindingsSchema::V1.code()));
        document.insert(
            "source".to_owned(),
            json!({
                "kind": SourceKind::Reviewer.code(),
                "name": REVIEWER,
                "version": env!("CARGO_PKG_VERSION"),
                "role": role.code(),
                "run_key": self.run_key.to_string(),
            }),
        );
        Value::Object(document)
    }
}

/// Whether the session that answered was confined to `Read`, `Grep` and `Glob` with no MCP
/// server: the part of the clean-init rule (03 §8) that holds whatever CLI version the developer
/// runs. Its output is not used otherwise.
fn confined(init: &Result<SessionInit, InitReadError>) -> Result<(), String> {
    let init = init
        .as_ref()
        .map_err(|error| format!("the session's init could not be read: {error}"))?;
    let mut tools: Vec<&str> = init.tools().iter().map(String::as_str).collect();
    tools.sort_unstable();
    if tools != CLEAN_TOOLS || !init.mcp_servers().is_empty() {
        return Err("the session was not confined to Read, Grep and Glob".to_owned());
    }
    Ok(())
}
