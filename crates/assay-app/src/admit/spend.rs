//! What `admit` does once it holds a slot (06 §1, R3-06, R3-14, R3-27): the budget read again at
//! the acquisition, since the wait may have lasted ten minutes and other runs may have reserved
//! in it; then the `reserved` record, the retry grant, the `Admitted` event and the run
//! description. When the read again defers the run its slot is released by compare-and-set and
//! the run is `budget-deferred`; when any step fails the slot is released the same way.

use assay_domain::{Aeu, EventPayload, RunUid};

use super::budget::SpendTier;
use super::document::aeu_number;
use super::error::{AfterSlot, UnwoundOf};
use super::events::EventFailure;
use super::gate::{Gate, GateInputs, Priced, gate};
use super::{
    Admission, AdmitError, AdmitFailure, Admitted, Budget, BudgetRead, CommandComments,
    ContextPack, Deferred, Run, Spend, Unwound, clock_time,
};
use crate::clock::Clock;
use crate::events::EventSink;
use crate::ledger::{
    Appended, Consumed, CostKind, CostRecord, Ledger, LedgerRefs, Released, SlotHold, append_cost,
    consume_grant, release_slot,
};
use crate::prompt_bundle::Role;
use crate::replay::BundleSource;

/// A run holding a slot, before the budget is read again.
pub(super) struct Holding {
    /// The slot.
    pub(super) hold: SlotHold,
    /// The runs whose expired leases the attempts at the slot reaped.
    pub(super) reaped: Vec<RunUid>,
    /// The tier the plan spends at.
    pub(super) tier: SpendTier,
    /// The run's context pack.
    pub(super) pack: ContextPack,
}

/// What a run the budget passed, read again at the acquisition, brings to its reservation.
struct Admittable {
    /// The tier, reservation and grant it was priced at.
    priced: Priced,
    /// Its context pack.
    pack: ContextPack,
    /// The AEU spent or reserved in the last 24 hours, its own lease aside.
    spent: Aeu,
}

/// A step after the slot that failed, and the grant consumed before it.
struct Late<L, R, S> {
    cause: Unwound<L, R, S>,
    consumed: Option<Role>,
}

/// The [`Late`] of a run over the ports' error types.
type LateOf<L, R, S> =
    Late<<L as Ledger>::Error, <R as LedgerRefs>::Error, <S as EventSink>::Error>;

impl<L, R, S> From<Unwound<L, R, S>> for Late<L, R, S> {
    fn from(cause: Unwound<L, R, S>) -> Self {
        Self {
            cause,
            consumed: None,
        }
    }
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
    /// Holding a slot: the budget read again, then the reservation or the deferral.
    pub(super) fn after_slot(
        mut self,
        holding: Holding,
    ) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        let Holding {
            hold,
            reaped,
            tier,
            pack,
        } = holding;
        if let Err(failure) = self.reaped(&reaped) {
            return Err(unwind(self.refs, hold, failure.unwound().into()));
        }
        let (gated, spent) = match self.recheck(tier) {
            Ok(gated) => gated,
            Err(cause) => return Err(unwind(self.refs, hold, cause.into())),
        };
        match gated {
            Gate::Spend(priced) => {
                let admittable = Admittable {
                    priced,
                    pack,
                    spent,
                };
                self.spend(hold, admittable)
            }
            Gate::Defer => self.give_back(hold, spent),
        }
    }

    /// The budget gate on the ledger as it is now, with this run's own lease left out.
    fn recheck(&self, tier: SpendTier) -> Result<(Gate, Aeu), UnwoundOf<L, R, S>> {
        let now = clock_time(&*self.clock).map_err(|_range| Unwound::Clock)?;
        let view = self.read_budget(now).map_err(|failure| match failure {
            BudgetRead::Costs(error) => Unwound::Costs(error),
            BudgetRead::Slots(error) => Unwound::Slots(error),
        })?;
        let inputs = GateInputs {
            planned: &self.planned,
            tier,
            repo_id: self.repo_id(),
            spent: view.spent,
            admissions: view.admissions,
        };
        let gated = gate(self.refs, inputs).map_err(Unwound::Grant)?;
        Ok((gated, view.spent))
    }

    /// A run the budget defers once it holds `hold`: the slot released, then the deferral.
    fn give_back(
        mut self,
        hold: SlotHold,
        spent: Aeu,
    ) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        match release_slot(self.refs, hold.slot, &hold.lease_commit) {
            Ok(Released::Released) => {
                self.recorder
                    .emit(EventPayload::SlotReleased {})
                    .map_err(EventFailure::admit)?;
            }
            Ok(Released::NotHeld) => {}
            Err(release) => {
                return Err(AdmitError::AfterSlot(Box::new(AfterSlot {
                    hold,
                    release: Some(release),
                    consumed: None,
                    cause: Unwound::Deferred,
                })));
            }
        }
        self.deferred(Deferred::Budget, spent)
    }

    /// Holding `hold`: the reservation, the grant and the outputs, releasing the slot when any
    /// of them fails.
    fn spend(
        mut self,
        hold: SlotHold,
        admittable: Admittable,
    ) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        let Admittable {
            priced,
            pack,
            spent,
        } = admittable;
        let retry = match self.reserve(&hold, &priced) {
            Ok(retry) => retry,
            Err(late) => return Err(unwind(self.refs, hold, late)),
        };
        let budget = Budget {
            day_spent: spent,
            allowed: true,
            slot: Some(hold.slot),
            deferred: None,
            degrade: priced.degrade,
            reused: None,
            retry,
        };
        let (refs, validator) = (self.refs, self.validator);
        let run_json = match self.planned.into_json(&budget, validator) {
            Ok(json) => json,
            Err(cause) => {
                let late = Late {
                    cause: Unwound::Plan(cause),
                    consumed: retry,
                };
                return Err(unwind(refs, hold, late));
            }
        };
        Ok(Admitted {
            run_json,
            admission: Admission::Spend(Spend {
                hold,
                tier: priced.tier.tier(),
                reserved: priced.reservation.total,
                retry,
            }),
            budget: Some(budget),
            pack: Some(pack),
        })
    }

    /// The `reserved` record, the retry grant and the `Admitted` event; the role whose grant
    /// this run consumed.
    fn reserve(
        &mut self,
        hold: &SlotHold,
        priced: &Priced,
    ) -> Result<Option<Role>, LateOf<L, R, S>> {
        let at = clock_time(&*self.clock).map_err(|_range| Unwound::Clock)?;
        let planned = &self.planned;
        // The record owns its fields.
        let record = CostRecord {
            run_uid: self.request.run_uid,
            run_key: planned.run_key.clone(),
            repo: planned.repo.clone(),
            pr: planned.pr,
            head_sha: planned.head.clone(),
            tier: priced.tier.tier(),
            roles: priced.reservation.roles.clone(),
            aeu: priced.reservation.total,
            kind: CostKind::Reserved,
            cancelled: false,
            at,
        };
        if append_cost(self.ledger, &record).map_err(Unwound::Reserve)? == Appended::Recorded {
            self.recorder
                .emit(EventPayload::LedgerWritten {})
                .map_err(EventFailure::unwound)?;
        }
        let retry = match &priced.grant {
            Some(key) => match consume_grant(self.refs, key).map_err(Unwound::Grant)? {
                Consumed::Consumed(grant) => Some(grant.role),
                Consumed::NoGrant => None,
            },
            None => None,
        };
        self.recorder
            .emit(EventPayload::Admitted {
                slot: format!("slot-{}", hold.slot),
                reserved_aeu: aeu_number(priced.reservation.total),
            })
            .map_err(|failure| Late {
                cause: failure.unwound(),
                consumed: retry,
            })?;
        Ok(retry)
    }

    /// One `SlotReaped` event per reaped lease.
    pub(super) fn reaped(&mut self, reaped: &[RunUid]) -> Result<(), EventFailure<S::Error>> {
        reaped.iter().try_for_each(|run_uid| {
            self.recorder
                .emit(EventPayload::SlotReaped { run_uid: *run_uid })
        })
    }
}

/// `late`'s cause after releasing `hold` by compare-and-set; a release that fails is kept beside
/// it, as is the grant consumed before it.
fn unwind<L, RE, B, C, S, R>(
    refs: &R,
    hold: SlotHold,
    late: Late<L, RE, S>,
) -> AdmitError<L, RE, B, C, S>
where
    R: LedgerRefs<Error = RE>,
{
    let release = release_slot(refs, hold.slot, &hold.lease_commit).err();
    AdmitError::AfterSlot(Box::new(AfterSlot {
        hold,
        release,
        consumed: late.consumed,
        cause: late.cause,
    }))
}
