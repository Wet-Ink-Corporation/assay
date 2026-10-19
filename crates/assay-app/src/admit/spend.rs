//! What `admit` does once it holds a slot (06 §1, R3-06, R3-14): the `reserved` record, the
//! retry grant, the `Admitted` event and the run description, releasing the slot by
//! compare-and-set when any of them fails.

use assay_domain::{Aeu, EventPayload, RunUid};

use super::document::aeu_number;
use super::error::{AfterSlot, UnwoundOf};
use super::events::EventFailure;
use super::gate::Priced;
use super::{
    Admission, AdmitError, AdmitFailure, Admitted, Budget, CommandComments, ContextPack, Run,
    Spend, Unwound, clock_time,
};
use crate::clock::Clock;
use crate::events::EventSink;
use crate::ledger::{
    Appended, Consumed, CostKind, CostRecord, Ledger, LedgerRefs, SlotHold, append_cost,
    consume_grant, release_slot,
};
use crate::prompt_bundle::Role;
use crate::replay::BundleSource;

/// What a run the budget gate passed brings to its slot.
pub(super) struct Admittable {
    /// The tier, reservation and grant it was priced at.
    pub(super) priced: Priced,
    /// Its context pack.
    pub(super) pack: ContextPack,
    /// The AEU spent or reserved in the last 24 hours.
    pub(super) spent: Aeu,
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
    /// Holding `hold`: the reservation, the grant and the outputs, releasing the slot when any
    /// of them fails.
    pub(super) fn spend(
        mut self,
        hold: SlotHold,
        reaped: &[RunUid],
        admittable: Admittable,
    ) -> Result<Admitted, AdmitFailure<L, R, B, C, S>> {
        let Admittable {
            priced,
            pack,
            spent,
        } = admittable;
        let retry = match self.reserve(&hold, reaped, &priced) {
            Ok(retry) => retry,
            Err(cause) => return Err(unwind(self.refs, hold, cause)),
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
            Err(cause) => return Err(unwind(refs, hold, Unwound::Plan(cause))),
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
        reaped: &[RunUid],
        priced: &Priced,
    ) -> Result<Option<Role>, UnwoundOf<L, R, S>> {
        self.reaped(reaped).map_err(EventFailure::unwound)?;
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
            .map_err(EventFailure::unwound)?;
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

/// `cause` after releasing `hold` by compare-and-set; a release that fails is kept beside it.
fn unwind<L, RE, B, C, S, R>(
    refs: &R,
    hold: SlotHold,
    cause: Unwound<L, RE, S>,
) -> AdmitError<L, RE, B, C, S>
where
    R: LedgerRefs<Error = RE>,
{
    let release = release_slot(refs, hold.slot, &hold.lease_commit).err();
    AdmitError::AfterSlot(Box::new(AfterSlot {
        hold,
        release,
        cause,
    }))
}
