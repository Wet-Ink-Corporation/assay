//! The budget read `admit` makes (06 §1): the last 24 hours of cost records plus the
//! reservations of leases not yet reaped, and the per-PR cooldown's count of admissions.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::num::NonZeroU64;
use std::time::Duration;

use assay_domain::{Aeu, RunUid};

use super::cas::read_file;
use super::cost::{CostKind, CostRecord, parse_costs};
use super::error::LedgerReadError;
use super::port::{Ledger, LedgerPath};
use super::time::LedgerTime;

/// How far back the daily cap looks.
pub const BUDGET_WINDOW: Duration = Duration::from_hours(24);

/// How far back the per-PR cooldown counts admissions (3 per hour, T12).
pub const COOLDOWN_WINDOW: Duration = Duration::from_hours(1);

/// What a slot's lease reserves for its run: the lease `{run_uid, reserved_aeu}` of a live
/// lease, or of an expired one not yet reaped (06 §1, #71).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseReservation {
    /// The run holding the slot.
    pub run_uid: RunUid,
    /// What its admission reserved.
    pub reserved_aeu: Aeu,
}

/// The cost records of the months the budget window covers, as read at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostWindow {
    now: LedgerTime,
    records: Vec<CostRecord>,
}

/// Reads the cost files the window ending `now` covers: `now`'s month, and the month before when
/// [`BUDGET_WINDOW`] reaches into it.
///
/// # Errors
///
/// [`LedgerReadError`] when a file cannot be read, is over its cap, or breaks a rule: the budget
/// is then unknown and `admit` fails (06 §1, F16).
pub fn read_costs<L: Ledger>(
    ledger: &L,
    now: LedgerTime,
) -> Result<CostWindow, LedgerReadError<L::Error>> {
    let first = now.saturating_sub(BUDGET_WINDOW).month();
    let last = now.month();
    let months = if first == last {
        vec![last]
    } else {
        vec![first, last]
    };
    let mut records = Vec::new();
    for month in months {
        let path = LedgerPath::costs(month);
        let Some(file) = read_file(ledger, &path)? else {
            continue;
        };
        let parsed = parse_costs(month, &file.contents).map_err(|(line, problem)| {
            LedgerReadError::Corrupt {
                path,
                line,
                problem,
            }
        })?;
        records.extend(parsed);
    }
    Ok(CostWindow { now, records })
}

impl CostWindow {
    /// The AEU spent or reserved in the 24 hours up to the read, which `admit` compares with the
    /// daily cap.
    ///
    /// Each run counts once: its `final` record when one was read, else its `reserved` record,
    /// and only when that record was written at or after the window's start (a later one counts
    /// too). A lease adds its reservation unless its run is already counted, so a run whose
    /// `reserved` record was not written still counts while it holds a slot (R3-06, R3-27).
    #[must_use]
    pub fn day_spent(&self, leases: &[LeaseReservation]) -> Aeu {
        let start = self.now.saturating_sub(BUDGET_WINDOW);
        let mut settled: HashMap<RunUid, &CostRecord> = HashMap::with_capacity(self.records.len());
        for record in &self.records {
            match settled.entry(record.run_uid) {
                Entry::Vacant(slot) => {
                    slot.insert(record);
                }
                Entry::Occupied(mut slot) => {
                    if record.kind == CostKind::Final {
                        slot.insert(record);
                    }
                }
            }
        }
        let recorded = settled
            .values()
            .filter(|record| record.at >= start)
            .fold(Aeu::ZERO, |sum, record| sum.saturating_add(record.aeu));
        leases
            .iter()
            .filter(|lease| {
                settled
                    .get(&lease.run_uid)
                    .is_none_or(|record| record.at < start)
            })
            .fold(recorded, |sum, lease| {
                sum.saturating_add(lease.reserved_aeu)
            })
    }

    /// How many times PR `pr` of repository `repo_id` was admitted in the hour up to the read:
    /// its `reserved` records written at or after the hour's start, whether or not they were
    /// settled since (R3-06).
    #[must_use]
    pub fn admissions(&self, repo_id: NonZeroU64, pr: NonZeroU64) -> usize {
        let start = self.now.saturating_sub(COOLDOWN_WINDOW);
        self.records
            .iter()
            .filter(|record| {
                record.kind == CostKind::Reserved
                    && record.run_uid.repo_id == repo_id
                    && record.pr == pr
                    && record.at >= start
            })
            .count()
    }

    /// Every cost record read, in file order, the earlier month first.
    #[must_use]
    pub fn records(&self) -> &[CostRecord] {
        &self.records
    }
}
