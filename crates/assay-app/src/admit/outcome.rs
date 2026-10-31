//! What `admit` decided: the run's admission, the budget it reports in the run description, and
//! the documents it outputs.

use assay_domain::{Aeu, Deferral, NotRunReason, Tier};

use super::pack::ContextPack;
use crate::decide::TierDegrade;
use crate::ledger::{Slot, SlotHold};
use crate::prompt_bundle::Role;
use crate::publish::EvidenceRecord;

/// What `admit` decided for the LLM stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The run has no LLM stage to admit (a draft, `trivial` or `needs-split`): no ledger read,
    /// no slot, no reservation (04 §2.4, 06 §1).
    NoSpend(NotRunReason),
    /// A complete evidence record for the run's evidence key, at a tier at least the one needed,
    /// stands in for the LLM stage: nothing is reserved or spent (02 §5, R3-15, R3-17).
    Reused(EvidenceRecord),
    /// The LLM stage is deferred; the ledger's deferral record says so (06 §1, R3-15).
    Deferred(Deferred),
    /// The run holds a slot and its reservation is in the ledger: the review may spend.
    Spend(Spend),
}

/// Why `admit` deferred the LLM stage: two different reasons, never one for the other (R3-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deferred {
    /// The day's cap or the PR's hourly admissions would be passed (`budget-deferred`).
    Budget,
    /// Every slot stayed held through the wait, or the slot taken reserved less than any price
    /// the run could spend at, and was given back (`slot-busy`).
    SlotBusy,
}

impl Deferred {
    /// The deferral `decide`'s rule 8a reads: on an agent-authored PR a `slot-busy` stays
    /// `pending` and requeues, never an `llm-degraded` success (R3-06).
    #[must_use]
    pub const fn deferral(self) -> Deferral {
        match self {
            Self::Budget => Deferral::BudgetDeferred,
            Self::SlotBusy => Deferral::SlotBusy,
        }
    }

    /// The `budget.deferral` code (03 §4).
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::Budget => "budget-deferred",
            Self::SlotBusy => "slot-busy",
        }
    }
}

/// A run admitted to spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spend {
    /// The slot held, which the `release` job releases (`slot` and `lease_commit` outputs).
    pub hold: SlotHold,
    /// The tier the LLM stage runs at: `lite` after a full-to-lite degrade.
    pub tier: Tier,
    /// What the `reserved` record and the lease reserve.
    pub reserved: Aeu,
    /// The role whose retry grant this run consumed, and whose cap is raised by half (R3-14).
    pub retry: Option<Role>,
}

/// The `budget` `admit` adds to the run description (03 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    /// The AEU spent or reserved in the 24 hours before the read (`ledger_day_spent_aeu`).
    pub day_spent: Aeu,
    /// Whether the budget gate passed: the day's cap and the PR's hourly admissions admit the
    /// run. False when a reusable record meant the gate was not evaluated.
    pub allowed: bool,
    /// The slot taken.
    pub slot: Option<Slot>,
    /// Why the LLM stage is deferred.
    pub deferred: Option<Deferred>,
    /// The full-to-lite degrade (`tier_degraded_from`, R3-27).
    pub degrade: TierDegrade,
    /// The evidence record reused (`reused_evidence`).
    pub reused: Option<EvidenceRecord>,
    /// The role whose retry grant was consumed (`retry_grant`).
    pub retry: Option<Role>,
}

/// What `admit` outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub(super) run_json: Vec<u8>,
    pub(super) admission: Admission,
    pub(super) budget: Option<Budget>,
    pub(super) pack: Option<ContextPack>,
}

impl Admitted {
    /// The run description: the plan's, with `budget` added unless the run has no LLM stage,
    /// validated as `assay.run/1`.
    #[must_use]
    pub fn run_json(&self) -> &[u8] {
        &self.run_json
    }

    /// What was decided for the LLM stage.
    #[must_use]
    pub const fn admission(&self) -> &Admission {
        &self.admission
    }

    /// The budget the run description carries; `None` when the run has no LLM stage.
    #[must_use]
    pub const fn budget(&self) -> Option<&Budget> {
        self.budget.as_ref()
    }

    /// The Z4 half of the context pack, built only for a run admitted to spend.
    #[must_use]
    pub const fn pack(&self) -> Option<&ContextPack> {
        self.pack.as_ref()
    }

    /// The degrade `decide` reads from `admit`'s outputs (R3-27).
    #[must_use]
    pub fn degrade(&self) -> TierDegrade {
        self.budget
            .as_ref()
            .map_or(TierDegrade::None, |budget| budget.degrade)
    }
}
