//! The `admit` use case (S1a, `docs/architecture/04-pipeline.md` §1, §2.4, 06 §1, 03 §4): whether
//! the LLM stage may spend, and what it takes to.
//!
//! In order: refuse a re-run (R3-05); read the plan's `assay.run/1`; stop for a run with no LLM
//! stage (a draft, `trivial`, `needs-split`) with nothing read or taken; read the PR's ledger
//! state, the day's cost records and the slots; reuse a complete evidence record for the run's
//! evidence key, at a tier at least the one needed, with nothing reserved (R3-15, R3-17); apply
//! the budget gate (the PR's hourly admissions, the day's cap, a classless `full` degraded to
//! `lite`, 04 §2.4); build the Z4 half of the context pack; take a slot, reaping expired leases
//! by compare-and-set before waiting up to ten minutes (R3-06); holding it, read the costs and
//! slots again (its own lease left out) and apply the budget gate again, so reservations made
//! during the wait count, releasing the slot by compare-and-set and deferring for budget if that
//! defers (R3-27); write the `reserved` record;
//! consume a waiting retry grant (R3-14); and output the run description with `budget`.
//!
//! A deferral (`budget-deferred` or `slot-busy`, never one for the other) writes the ledger's
//! deferral record (R3-15). Every read happens before any write, and any failure fails the job,
//! so `decide` applies the LLM-unavailable policy (F16); a failure after the slot was taken
//! releases it by compare-and-set. The live GitHub transport behind the ports is #273's.

mod budget;
mod dismissals;
mod document;
mod error;
mod events;
mod gate;
mod outcome;
mod pack;
mod range;
mod slot;
mod spend;

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use assay_domain::{Aeu, EventPayload, NotRunReason, RunUid};

use self::budget::SpendTier;
pub use self::budget::{ADMISSIONS_PER_HOUR, DAY_CAP};
pub use self::dismissals::MAX_DISMISSALS;
use self::dismissals::{Eligible, valid_dismissals};
pub use self::document::PlanDocument;
use self::document::Planned;
pub use self::error::{AdmitError, AdmitFailure, AfterSlot, Unwound};
use self::events::{EventFailure, Recorder};
use self::gate::{Gate, GateInputs, gate, reusable};
pub use self::outcome::{Admission, Admitted, Budget, Deferred, Spend};
pub use self::pack::{ContextPack, MAX_SUPPRESS_BYTES};
use self::pack::{PackParts, pack};
pub use self::range::RangeIdentities;
pub use self::slot::{SLOT_POLL, SLOT_POLLS};
use self::slot::{SlotFailure, take_slot};
use self::spend::Holding;
use crate::clock::Clock;
use crate::contract::ContractValidator;
use crate::decide::TierDegrade;
use crate::eligibility::CommandComments;
use crate::events::EventSink;
use crate::ledger::{
    DeferralRecord, LeaseError, Ledger, LedgerReadError, LedgerRefs, LedgerTime,
    ParseLedgerTimeError, PrState, PushKind, SlotRequest, SlotView, StateUpdate, read_commands,
    read_costs, read_pushes, read_slots, read_state, update_state,
};
use crate::publish::EvidenceRecord;
use crate::replay::BundleSource;
use crate::rereview::{Carry, PriorCarry, prior_carry};
use crate::status::{Identities, UserId};

/// The ports `admit` reads and writes, borrowed for one call.
#[derive(Debug)]
pub struct AdmitPorts<'p, L, R, B, C, K, S> {
    /// The ledger's files: state, cost records, push and command records.
    pub ledger: &'p L,
    /// The ledger's refs: slots and retry grants.
    pub refs: &'p R,
    /// The run artifacts, for the previous run's bundle.
    pub bundles: &'p B,
    /// The PR's issue comments, to void a command record.
    pub comments: &'p C,
    /// The wall clock, and the slot wait.
    pub clock: &'p mut K,
    /// The job's event stream.
    pub events: &'p mut S,
}

/// What `admit` is given besides its ports.
#[derive(Debug, Clone, Copy)]
pub struct AdmitRequest<'a> {
    /// This workflow run, from the environment; the plan must name it.
    pub run_uid: RunUid,
    /// The plan's `run.json`, already authenticated by the digest the `plan` job published.
    pub plan: &'a [u8],
    /// Whether the previous completed run's open findings carry (`review_target`, 04 §7).
    pub carry: Carry,
    /// The base P10 identity lists.
    pub identities: &'a Identities,
    /// The PR's author, from the triggering event.
    pub author: UserId,
    /// The authors and committers of the PR's range (04 §5 clause (b)).
    pub range: &'a RangeIdentities,
    /// The base `.assay/suppress.toml` text read at the pinned base, if the file exists; carried
    /// in the pack as it is.
    pub suppress: Option<&'a str>,
}

/// Decides and takes what the run's LLM stage needs: see the module documentation.
///
/// # Errors
///
/// [`AdmitError::NotFirstAttempt`] before anything is read; [`AdmitError::Plan`] when the
/// plan's document is refused; a read failure ([`AdmitError::LedgerRead`],
/// [`AdmitError::Lease`], [`AdmitError::Prior`], [`AdmitError::NoPriorPointer`],
/// [`AdmitError::Comments`]) or an oversized suppress text before anything is written;
/// [`AdmitError::LedgerWrite`] when a deferral record cannot be written; event and clock
/// failures; and [`AdmitError::AfterSlot`] for a failure once a slot was taken.
pub fn admit<L, R, B, C, K, S>(
    ports: AdmitPorts<'_, L, R, B, C, K, S>,
    request: &AdmitRequest<'_>,
    validator: &ContractValidator,
) -> Result<Admitted, AdmitFailure<L, R, B, C, S>>
where
    L: Ledger,
    R: LedgerRefs,
    B: BundleSource,
    C: CommandComments,
    K: Clock,
    S: EventSink,
{
    let attempt = request.run_uid.run_attempt;
    if attempt.get() != 1 {
        return Err(AdmitError::NotFirstAttempt { attempt });
    }
    let planned =
        Planned::read(validator, request.plan, request.run_uid).map_err(AdmitError::Plan)?;
    let tier = match (planned.draft, SpendTier::of(planned.tier)) {
        (true, _) => return Ok(no_spend(request.plan, NotRunReason::Draft)),
        (false, None) => return Ok(no_spend(request.plan, NotRunReason::NoLlmTier)),
        (false, Some(tier)) => tier,
    };
    let AdmitPorts {
        ledger,
        refs,
        bundles,
        comments,
        clock,
        events,
    } = ports;
    let recorder = Recorder::new(events, planned.run_key.to_string(), request.run_uid);
    let run = Run {
        ledger,
        refs,
        bundles,
        comments,
        clock,
        recorder,
        request,
        validator,
        planned,
    };
    run.admit(tier)
}

/// The plan's document unchanged: a run with no LLM stage has nothing to admit.
fn no_spend(plan: &[u8], reason: NotRunReason) -> Admitted {
    Admitted {
        run_json: plan.to_vec(),
        admission: Admission::NoSpend(reason),
        budget: None,
        pack: None,
    }
}

/// The ledger as `admit` read it.
struct LedgerView {
    state: PrState,
    spent: Aeu,
    admissions: usize,
}

/// What the budget gate reads from the ledger.
struct BudgetView {
    /// The AEU spent or reserved in the last 24 hours, this run's own lease aside.
    spent: Aeu,
    /// The PR's `reserved` records in the last hour.
    admissions: usize,
}

/// Why the budget could not be read.
enum BudgetRead<L, R> {
    /// The cost records.
    Costs(LedgerReadError<L>),
    /// The slots.
    Slots(LeaseError<R>),
}

/// One admission in progress: the ports, the request and the plan.
struct Run<'p, 'r, L, R, B, C, K, S> {
    ledger: &'p L,
    refs: &'p R,
    bundles: &'p B,
    comments: &'p C,
    clock: &'p mut K,
    recorder: Recorder<'p, S>,
    request: &'r AdmitRequest<'r>,
    validator: &'r ContractValidator,
    planned: Planned,
}

impl<L, R, B, C, K, S> Run<'_, '_, L, R, B, C, K, S>
where
    L: Ledger,
    R: LedgerRefs,
    B: BundleSource,
    C: CommandComments,
    K: Clock,
    S: EventSink,
{
    /// The run's repository id.
    const fn repo_id(&self) -> NonZeroU64 {
        self.request.run_uid.repo_id
    }

    /// Everything after the plan was read, for a run whose LLM stage spends at `tier`.
    fn admit(mut self, tier: SpendTier) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        let mut view = self.read()?;
        if let Some(index) = reusable(&view.state, &self.planned) {
            let record = view.state.evidence_records.swap_remove(index);
            return self.reused(record, view.spent);
        }
        let inputs = GateInputs {
            planned: &self.planned,
            tier,
            repo_id: self.repo_id(),
            spent: view.spent,
            admissions: view.admissions,
        };
        let Gate::Spend(priced) = gate(self.refs, inputs).map_err(AdmitError::Lease)? else {
            return self.deferred(Deferred::Budget, view.spent);
        };
        let pack = self.pack(&view.state)?;
        let slot_request = SlotRequest {
            run_uid: self.request.run_uid,
            // The lease stores the repository.
            repo: self.planned.repo.clone(),
            pr: self.planned.pr,
            reserved_aeu: priced.reservation.total,
        };
        let taken =
            take_slot(self.refs, self.clock, &slot_request).map_err(|failure| match failure {
                SlotFailure::Lease(error) => AdmitError::Lease(error),
                SlotFailure::Clock => AdmitError::Clock,
            })?;
        match taken.hold {
            None => {
                self.reaped(&taken.reaped).map_err(EventFailure::admit)?;
                self.deferred(Deferred::SlotBusy, view.spent)
            }
            Some(hold) => {
                let holding = Holding {
                    hold,
                    reaped: taken.reaped,
                    tier,
                    pack,
                };
                self.after_slot(holding)
            }
        }
    }

    /// The PR's state, the day's spend and the PR's admissions in the hour, read at one instant.
    fn read(&self) -> Result<LedgerView, AdmitFailure<L, R, B, C, S>> {
        let now = clock_time(&*self.clock).map_err(|_range| AdmitError::Clock)?;
        let (repo_id, pr) = (self.repo_id(), self.planned.pr);
        let state = read_state(self.ledger, repo_id, pr).map_err(AdmitError::LedgerRead)?;
        let budget = self.read_budget(now).map_err(|failure| match failure {
            BudgetRead::Costs(error) => AdmitError::LedgerRead(error),
            BudgetRead::Slots(error) => AdmitError::Lease(error),
        })?;
        Ok(LedgerView {
            state,
            spent: budget.spent,
            admissions: budget.admissions,
        })
    }

    /// The day's spend and the PR's admissions in the hour at `now`, from the cost records and
    /// the slots' leases; a lease of this run is left out, since its reservation is the one being
    /// decided.
    fn read_budget(&self, now: LedgerTime) -> Result<BudgetView, BudgetRead<L::Error, R::Error>> {
        let run_uid = self.request.run_uid;
        let costs = read_costs(self.ledger, now).map_err(BudgetRead::Costs)?;
        let slots = read_slots(self.refs, now).map_err(BudgetRead::Slots)?;
        let leases: Vec<_> = slots
            .iter()
            .map(SlotView::reservation)
            .filter(|lease| lease.run_uid != run_uid)
            .collect();
        Ok(BudgetView {
            spent: costs.day_spent(&leases),
            admissions: costs.admissions(run_uid.repo_id, self.planned.pr),
        })
    }

    /// A reused evidence record: nothing reserved or spent.
    fn reused(
        mut self,
        record: EvidenceRecord,
        spent: Aeu,
    ) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        self.recorder
            .emit(EventPayload::EvidenceReused {
                evidence_key: record.evidence_key.clone(),
            })
            .map_err(EventFailure::admit)?;
        let budget = Budget {
            day_spent: spent,
            allowed: false,
            slot: None,
            deferred: None,
            degrade: TierDegrade::None,
            // The budget and the admission each report the record.
            reused: Some(record.clone()),
            retry: None,
        };
        self.finish(Admission::Reused(record), budget, None)
    }

    /// A deferral: the ledger's deferral record for the plan's head, then its events.
    fn deferred(
        mut self,
        deferred: Deferred,
        spent: Aeu,
    ) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        let planned = &self.planned;
        let update = update_state(self.ledger, self.repo_id(), planned.pr, |state| {
            with_deferral(state, planned)
        })
        .map_err(AdmitError::LedgerWrite)?;
        let kind = match deferred {
            Deferred::Budget => EventPayload::BudgetDeferred {},
            Deferred::SlotBusy => EventPayload::SlotBusy {},
        };
        self.recorder.emit(kind).map_err(EventFailure::admit)?;
        if let StateUpdate::Updated(_) = update {
            self.recorder
                .emit(EventPayload::LedgerWritten {})
                .map_err(EventFailure::admit)?;
        }
        let budget = Budget {
            day_spent: spent,
            allowed: deferred == Deferred::SlotBusy,
            slot: None,
            deferred: Some(deferred),
            degrade: TierDegrade::None,
            reused: None,
            retry: None,
        };
        self.finish(Admission::Deferred(deferred), budget, None)
    }

    /// The context pack: the suppress text, the dismissals that stand and the prior findings.
    fn pack(&self, state: &PrState) -> Result<ContextPack, AdmitFailure<L, R, B, C, S>> {
        let request = self.request;
        if let Some(text) = request.suppress
            && text.len() > MAX_SUPPRESS_BYTES
        {
            return Err(AdmitError::SuppressTooLarge { size: text.len() });
        }
        let (repo_id, pr) = (self.repo_id(), self.planned.pr);
        // An opener record's sender opened the PR and is not a pusher (03 §6c), and a push of a
        // clean merge from the base does not count (04 §5).
        let range = request.range;
        let pushers: BTreeSet<UserId> = read_pushes(self.ledger, repo_id, pr)
            .map_err(AdmitError::LedgerRead)?
            .into_iter()
            .filter(|record| matches!(record.push.kind, PushKind::Push { .. }))
            .filter(|record| !range.is_clean_merge(&record.push.after))
            .map(|record| record.push.sender_id)
            .collect();
        let records = read_commands(self.ledger, repo_id, pr).map_err(AdmitError::LedgerRead)?;
        let eligible = Eligible {
            identities: request.identities,
            author: request.author,
            range,
            pushers: &pushers,
        };
        let dismissals =
            valid_dismissals(records, eligible, self.comments).map_err(AdmitError::Comments)?;
        let prior = match (request.carry, &state.evidence) {
            (Carry::Kept, None) => return Err(AdmitError::NoPriorPointer),
            (carry, Some(pointer)) => {
                let prior = PriorCarry {
                    repo_id,
                    pointer,
                    repository: &self.planned.repo,
                };
                prior_carry(carry, &prior, self.bundles).map_err(AdmitError::Prior)?
            }
            (Carry::FirstReview | Carry::Reset(_), None) => Vec::new(),
        };
        pack(PackParts {
            run_uid: request.run_uid,
            run_key: &self.planned.run_key,
            suppress: request.suppress,
            dismissals: &dismissals,
            prior: &prior,
        })
        .map_err(AdmitError::Pack)
    }

    /// The outputs of a run that takes no slot.
    fn finish(
        self,
        admission: Admission,
        budget: Budget,
        pack: Option<ContextPack>,
    ) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        let run_json = self
            .planned
            .into_json(&budget, self.validator)
            .map_err(AdmitError::Plan)?;
        Ok(Admitted {
            run_json,
            admission,
            budget: Some(budget),
            pack,
        })
    }
}

/// `state` with a deferral record for the plan's head: one already there for that head is kept
/// (its count, flag and reset time are the requeue's), otherwise a new one at count 0.
fn with_deferral(state: &PrState, planned: &Planned) -> Option<PrState> {
    if state
        .deferral
        .as_ref()
        .is_some_and(|deferral| deferral.head == planned.head)
    {
        return None;
    }
    // The update writes a new state from the one shown.
    let mut next = state.clone();
    next.deferral = Some(DeferralRecord {
        // The record stores the repository and head.
        repo: planned.repo.clone(),
        pr: planned.pr,
        head: planned.head.clone(),
        count: 0,
        resets_at: None,
        flag: false,
    });
    Some(next)
}

/// The clock's reading as a ledger time.
fn clock_time<K: Clock>(clock: &K) -> Result<LedgerTime, ParseLedgerTimeError> {
    LedgerTime::from_system_time(clock.now())
}
