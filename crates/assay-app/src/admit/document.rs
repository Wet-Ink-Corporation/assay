//! The run description `admit` reads from `plan` and writes back with `budget` (03 §4). The plan's
//! document is authenticated by its digest by the caller and validated as `assay.run/1` here;
//! `admit` changes only `budget`, and on a full-to-lite degrade `tier` and `roles`.

use std::num::NonZeroU64;

use assay_domain::{Aeu, PrClass, RequiredRole, RunUid, Tier};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use super::budget::DAY_CAP;
use super::outcome::Budget;
use crate::config::{CommitId, Sha256Digest};
use crate::contract::{Contract, ContractValidator, ReadContractError};
use crate::decide::TierDegrade;
use crate::publish::{EvidenceRecord, LlmEvidence};

/// Micro-AEU in one AEU.
const MICROS_PER_AEU: u64 = 1_000_000;

/// What `admit` observes of the subscription's rate-limit window: nothing. Only the review's
/// stream sees `rate_limit_event`s, and it pauses on them itself (06 §1).
const WINDOW_STATE: &str = "unobserved";

/// Why the plan's run description, or the one `admit` would write, is refused.
#[derive(Debug, Error)]
pub enum PlanDocument {
    /// It is not a valid `assay.run/1` document.
    #[error("the plan's run description is refused")]
    Refused(#[source] ReadContractError),
    /// It already carries a `budget`, which only `admit` adds.
    #[error("the plan's run description already carries a budget")]
    Budgeted,
    /// It describes another workflow run than this one.
    #[error("the plan's run description is of another run")]
    OtherRun {
        /// The run it names.
        planned: RunUid,
    },
    /// A field is not in the form `admit` reads.
    #[error("field {0} of the plan's run description is not in the form admit reads")]
    Field(&'static str),
    /// The document, or the `budget` added to it, does not convert to or from JSON in the form
    /// `admit` reads and writes.
    #[error("part {part} of the run description does not convert as admit needs")]
    Json {
        /// `run` for the plan's fields, `budget` for what `admit` adds.
        part: &'static str,
        /// What serde reported.
        #[source]
        source: serde_json::Error,
    },
    /// A run that is not a draft schedules other roles than its tier requires.
    #[error("the plan's roles are not the roles its tier requires")]
    Roles,
    /// The document `admit` wrote is not a valid `assay.run/1` document: a defect here.
    #[error("the run description admit wrote is refused")]
    Written(#[source] ReadContractError),
}

/// The plan's run description, and the fields `admit` acts on.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Planned {
    document: Map<String, Value>,
    /// The evidence key, `run_key`.
    pub(super) run_key: Sha256Digest,
    /// The repository, `owner/name`.
    pub(super) repo: String,
    /// The PR.
    pub(super) pr: NonZeroU64,
    /// The pinned head.
    pub(super) head: CommitId,
    /// The planned tier.
    pub(super) tier: Tier,
    /// The PR's classes.
    pub(super) classes: Vec<PrClass>,
    /// The roles the tier requires.
    pub(super) roles: Vec<RequiredRole>,
    /// Whether the PR is a draft.
    pub(super) draft: bool,
}

/// The fields `admit` reads; the schema has checked every field already.
#[derive(Deserialize)]
struct Fields {
    run_key: String,
    run_uid: RunUid,
    repo: String,
    pr: NonZeroU64,
    head_sha: String,
    tier: Tier,
    classes: Vec<PrClass>,
    roles: Vec<RequiredRole>,
    required_roles: Vec<RequiredRole>,
    draft: bool,
}

impl Planned {
    /// The plan's document `bytes`, refused unless it is a valid `assay.run/1` document of
    /// `run_uid` without a budget.
    pub(super) fn read(
        validator: &ContractValidator,
        bytes: &[u8],
        run_uid: RunUid,
    ) -> Result<Self, PlanDocument> {
        let value = validator
            .read(Contract::Run, bytes)
            .map_err(PlanDocument::Refused)?;
        let fields = Fields::deserialize(&value).map_err(|source| PlanDocument::Json {
            part: "run",
            source,
        })?;
        let Value::Object(document) = value else {
            return Err(PlanDocument::Field("run"));
        };
        if document.contains_key("budget") {
            return Err(PlanDocument::Budgeted);
        }
        if fields.run_uid != run_uid {
            return Err(PlanDocument::OtherRun {
                planned: fields.run_uid,
            });
        }
        if !fields.draft && fields.roles != fields.required_roles {
            return Err(PlanDocument::Roles);
        }
        Ok(Self {
            document,
            run_key: fields
                .run_key
                .parse()
                .map_err(|_key| PlanDocument::Field("run_key"))?,
            repo: fields.repo,
            pr: fields.pr,
            head: fields
                .head_sha
                .parse()
                .map_err(|_head| PlanDocument::Field("head_sha"))?,
            tier: fields.tier,
            classes: fields.classes,
            roles: fields.required_roles,
            draft: fields.draft,
        })
    }

    /// The document with `budget`, and on a degrade `tier: lite` and `roles: [combined]`,
    /// validated as `assay.run/1`.
    pub(super) fn into_json(
        self,
        budget: &Budget,
        validator: &ContractValidator,
    ) -> Result<Vec<u8>, PlanDocument> {
        let mut document = self.document;
        if budget.degrade == TierDegrade::FullToLite {
            document.insert("tier".to_owned(), Value::from(Tier::Lite.code()));
            document.insert(
                "roles".to_owned(),
                Value::from(vec![RequiredRole::Combined.code()]),
            );
        }
        let wire = BudgetWire::of(budget)?;
        let as_json = |source| PlanDocument::Json {
            part: "budget",
            source,
        };
        let budget = serde_json::to_value(wire).map_err(as_json)?;
        document.insert("budget".to_owned(), budget);
        let json = serde_json::to_vec_pretty(&document).map_err(as_json)?;
        validator
            .read(Contract::Run, &json)
            .map_err(PlanDocument::Written)?;
        Ok(json)
    }
}

/// `budget` as 03 §4 writes it (OUT-01).
#[derive(Serialize)]
struct BudgetWire<'a> {
    ledger_day_spent_aeu: f64,
    day_cap_aeu: f64,
    window_state: &'static str,
    allowed: bool,
    slot: Option<String>,
    deferral: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier_degraded_from: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reused_evidence: Option<ReusedWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_grant: Option<GrantWire>,
}

/// `budget.reused_evidence`: the ledger's evidence record, its digest as bare hex.
#[derive(Serialize)]
struct ReusedWire<'a> {
    evidence_key: &'a str,
    bundle_sha256: &'a str,
    llm: &'static str,
    tier: Tier,
}

/// `budget.retry_grant`.
#[derive(Serialize)]
struct GrantWire {
    role: &'static str,
}

impl<'a> BudgetWire<'a> {
    fn of(budget: &'a Budget) -> Result<Self, PlanDocument> {
        Ok(Self {
            ledger_day_spent_aeu: aeu_number(budget.day_spent),
            day_cap_aeu: aeu_number(DAY_CAP),
            window_state: WINDOW_STATE,
            allowed: budget.allowed,
            slot: budget.slot.map(|slot| format!("slot-{slot}")),
            deferral: budget.deferred.map_or("none", |deferred| deferred.code()),
            tier_degraded_from: (budget.degrade == TierDegrade::FullToLite)
                .then_some(Tier::Full.code()),
            reused_evidence: budget.reused.as_ref().map(reused).transpose()?,
            retry_grant: budget.retry.map(|role| GrantWire { role: role.code() }),
        })
    }
}

/// `record` as `budget.reused_evidence` writes it.
fn reused(record: &EvidenceRecord) -> Result<ReusedWire<'_>, PlanDocument> {
    Ok(ReusedWire {
        evidence_key: &record.evidence_key,
        bundle_sha256: record
            .bundle_sha256
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(PlanDocument::Field("reused_evidence"))?,
        llm: match record.llm {
            LlmEvidence::Complete => "complete",
            LlmEvidence::SkippedDraft => "skipped",
            LlmEvidence::Deferred => "deferred",
            LlmEvidence::Degraded => "degraded",
        },
        tier: record.tier,
    })
}

/// `amount` as the number the run description carries: whole AEU past `u32::MAX` (about four
/// billion dollars, far past any ledger the cap admits) read as `u32::MAX`, which still reads as
/// over the cap.
pub(super) fn aeu_number(amount: Aeu) -> f64 {
    let micros = amount.micros();
    let whole = u32::try_from(micros / MICROS_PER_AEU).unwrap_or(u32::MAX);
    // A remainder below 10^6 always fits.
    let fraction = u32::try_from(micros % MICROS_PER_AEU).unwrap_or(u32::MAX);
    f64::from(whole) + f64::from(fraction) / 1_000_000.0
}
