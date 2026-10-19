//! The decisions `admit` makes before it takes a slot (06 §1, 04 §2.4, 02 §5): whether a
//! complete evidence record stands in for the LLM stage, and whether the budget admits the run,
//! at its planned tier or degraded from `full` to `lite`.

use std::num::NonZeroU64;

use assay_domain::{Aeu, RequiredRole, Tier};

use super::budget::{ADMISSIONS_PER_HOUR, Reservation, SpendTier, fits, reservation};
use super::document::Planned;
use crate::decide::TierDegrade;
use crate::ledger::{GrantKey, LeaseError, LedgerRefs, PrState, pending_grant};
use crate::prompt_bundle::Role;
use crate::publish::LlmEvidence;

/// The roles a `lite` run requires (04 §3).
const LITE_ROLES: [RequiredRole; 1] = [RequiredRole::Combined];

/// What the budget gate decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Gate {
    /// The run may spend, as priced.
    Spend(Priced),
    /// The run is deferred for budget (`budget-deferred`).
    Defer,
}

/// A run priced at the tier it would spend at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Priced {
    /// The tier the LLM stage runs at.
    pub(super) tier: SpendTier,
    /// Whether that is a full-to-lite degrade.
    pub(super) degrade: TierDegrade,
    /// The grant waiting for one of its roles, reserved for at x1.5 (R3-14).
    pub(super) grant: Option<GrantKey>,
    /// What it reserves.
    pub(super) reservation: Reservation,
}

/// What the gate reads: the plan, the day's spend and the PR's admissions in the hour.
#[derive(Debug, Clone, Copy)]
pub(super) struct GateInputs<'a> {
    /// The plan.
    pub(super) planned: &'a Planned,
    /// The tier the plan spends at.
    pub(super) tier: SpendTier,
    /// The repository's id, which names grant refs.
    pub(super) repo_id: NonZeroU64,
    /// The AEU spent or reserved in the last 24 hours.
    pub(super) spent: Aeu,
    /// The PR's `reserved` records in the last hour.
    pub(super) admissions: usize,
}

/// Where the state's evidence records hold the complete record for the plan's evidence key at a
/// tier at least the plan's, if one exists: a draft, deferred or degraded record never stands in
/// for an LLM review (R3-17).
pub(super) fn reusable(state: &PrState, planned: &Planned) -> Option<usize> {
    let needed = rank(planned.tier)?;
    state.evidence_records.iter().position(|record| {
        record.evidence_key == planned.run_key.as_str()
            && record.llm == LlmEvidence::Complete
            && rank(record.tier).is_some_and(|reviewed| reviewed >= needed)
    })
}

/// How much review a tier's LLM stage gives; `None` for a tier without one.
const fn rank(tier: Tier) -> Option<u8> {
    match tier {
        Tier::Full => Some(3),
        Tier::Lite => Some(2),
        Tier::Micro => Some(1),
        Tier::Trivial | Tier::NeedsSplit => None,
    }
}

/// The budget gate: a PR admitted [`ADMISSIONS_PER_HOUR`] times in the hour is deferred; a run
/// whose reservation fits under the day's cap spends at its tier; a classless `full` run that
/// does not fit is tried as `lite`; anything else is deferred. A run with a class is never
/// degraded (04 §2.4).
///
/// # Errors
///
/// The lease error when a grant ref cannot be read.
pub(super) fn gate<R: LedgerRefs>(
    refs: &R,
    inputs: GateInputs<'_>,
) -> Result<Gate, LeaseError<R::Error>> {
    if inputs.admissions >= ADMISSIONS_PER_HOUR {
        return Ok(Gate::Defer);
    }
    let planned = inputs.planned;
    let as_planned = Offer {
        tier: inputs.tier,
        roles: &planned.roles,
        degrade: TierDegrade::None,
    };
    let at_tier = price(refs, inputs, as_planned)?;
    if fits(inputs.spent, at_tier.reservation.total) {
        return Ok(Gate::Spend(at_tier));
    }
    if inputs.tier == SpendTier::Full && planned.classes.is_empty() {
        let degraded = Offer {
            tier: SpendTier::Lite,
            roles: &LITE_ROLES,
            degrade: TierDegrade::FullToLite,
        };
        let lite = price(refs, inputs, degraded)?;
        if fits(inputs.spent, lite.reservation.total) {
            return Ok(Gate::Spend(lite));
        }
    }
    Ok(Gate::Defer)
}

/// A tier the run could spend at, with the roles it would run there.
#[derive(Debug, Clone, Copy)]
struct Offer<'a> {
    tier: SpendTier,
    roles: &'a [RequiredRole],
    degrade: TierDegrade,
}

/// The run priced as `offer`, a waiting grant for one of its roles included.
fn price<R: LedgerRefs>(
    refs: &R,
    inputs: GateInputs<'_>,
    offer: Offer<'_>,
) -> Result<Priced, LeaseError<R::Error>> {
    let grant = waiting_grant(refs, inputs, offer.roles)?;
    let reservation = reservation(offer.tier, offer.roles, grant.as_ref().map(|key| key.role));
    Ok(Priced {
        tier: offer.tier,
        degrade: offer.degrade,
        grant,
        reservation,
    })
}

/// The key of the first of `roles`, in roster order, with a grant waiting on the plan's head.
fn waiting_grant<R: LedgerRefs>(
    refs: &R,
    inputs: GateInputs<'_>,
    roles: &[RequiredRole],
) -> Result<Option<GrantKey>, LeaseError<R::Error>> {
    let planned = inputs.planned;
    let mut roster = roles.to_vec();
    roster.sort_unstable();
    roster.dedup();
    for role in roster {
        let key = GrantKey {
            repo_id: inputs.repo_id,
            pr: planned.pr,
            // The key owns its head.
            head_sha: planned.head.clone(),
            role: Role::from(role),
        };
        if pending_grant(refs, &key)?.is_some() {
            return Ok(Some(key));
        }
    }
    Ok(None)
}
