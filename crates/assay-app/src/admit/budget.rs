//! What a run reserves and whether the day's budget admits it (06 §1 "Caps", 04 §2.4).
//!
//! The reservation is the run cap of the tier the LLM stage runs at: the caps of its roles,
//! plus the coordinator and the verifier stage where the tier has them. `admit` cannot know how
//! many shards the review will split a `full` run into (the review builds the pack the split
//! needs), so a `full` run reserves for [`MAX_SHARDS`] shards: the reservation is never below
//! what the run may spend (fail closed, wi-b5affa).

use std::collections::BTreeSet;

use assay_domain::{Aeu, RequiredRole, Tier};

use crate::coordinate::COORDINATOR_CAP;
use crate::ledger::{CostRole, RoleCost};
use crate::prompt_bundle::Role;
use crate::shard::MAX_SHARDS;
use crate::verify::VerifierTier;

/// The daily cap per subscription (A-13, provisional): $40 AEU.
pub const DAY_CAP: Aeu = Aeu::from_micros(40_000_000);

/// How many times one PR may be admitted in an hour before it is deferred (06 §1, T12).
pub const ADMISSIONS_PER_HOUR: usize = 3;

/// A tier whose LLM stage spends: the only tiers `admit` reserves for, and the only ones
/// `plan` reports as reviewed (`crate::plan::reviewed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SpendTier {
    /// The full roster, the coordinator and the five-call verifier.
    Full,
    /// `combined` and the three-call verifier.
    Lite,
    /// `combined-micro` alone.
    Micro,
}

impl SpendTier {
    /// The tier `tier` spends at; `None` for `trivial` and `needs-split`, which have no LLM stage.
    pub(crate) const fn of(tier: Tier) -> Option<Self> {
        match tier {
            Tier::Full => Some(Self::Full),
            Tier::Lite => Some(Self::Lite),
            Tier::Micro => Some(Self::Micro),
            Tier::Trivial | Tier::NeedsSplit => None,
        }
    }

    /// The tier as the run description and the cost record name it.
    pub(super) const fn tier(self) -> Tier {
        match self {
            Self::Full => Tier::Full,
            Self::Lite => Tier::Lite,
            Self::Micro => Tier::Micro,
        }
    }

    /// How many shards the reservation covers.
    fn shards(self) -> u64 {
        match self {
            Self::Full => u64::try_from(MAX_SHARDS).unwrap_or(u64::MAX),
            Self::Lite | Self::Micro => 1,
        }
    }

    /// The verifier stage's tier.
    const fn verifier(self) -> VerifierTier {
        match self {
            Self::Full => VerifierTier::Full,
            Self::Lite => VerifierTier::Lite,
            Self::Micro => VerifierTier::Micro,
        }
    }
}

/// What a run reserves: each model-calling stage's cap, and their total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reservation {
    /// Reviewer roles in roster order, then the coordinator and the verifier where the tier
    /// has them; each at most once.
    pub(super) roles: Vec<RoleCost>,
    /// The sum of `roles`.
    pub(super) total: Aeu,
}

/// The reservation of a run at `tier` with `roles` (each counted once), `granted` the role whose
/// cap a retry grant raises by half (R3-14).
pub(super) fn reservation(
    tier: SpendTier,
    roles: &[RequiredRole],
    granted: Option<Role>,
) -> Reservation {
    let shards = tier.shards();
    let scaled = |cap: Aeu| Aeu::from_micros(cap.micros().saturating_mul(shards));
    let mut costs: Vec<RoleCost> = roles
        .iter()
        .copied()
        .collect::<BTreeSet<RequiredRole>>()
        .into_iter()
        .map(|role| {
            let role = Role::from(role);
            let cap = if granted == Some(role) {
                raised(role.cap())
            } else {
                role.cap()
            };
            RoleCost {
                role: CostRole::Role(role),
                aeu: scaled(cap),
            }
        })
        .collect();
    if tier == SpendTier::Full {
        costs.push(RoleCost {
            role: CostRole::Coordinator,
            aeu: scaled(COORDINATOR_CAP),
        });
    }
    if tier != SpendTier::Micro {
        costs.push(RoleCost {
            role: CostRole::Verifier,
            aeu: scaled(tier.verifier().budget()),
        });
    }
    let total = costs
        .iter()
        .fold(Aeu::ZERO, |sum, cost| sum.saturating_add(cost.aeu));
    Reservation {
        roles: costs,
        total,
    }
}

/// `cap` raised by half, as a retry grant raises its role's (06 §1).
fn raised(cap: Aeu) -> Aeu {
    let micros = cap.micros();
    Aeu::from_micros(micros.saturating_add(micros / 2))
}

/// Whether `reservation` on top of `spent` stays within [`DAY_CAP`].
pub(super) fn fits(spent: Aeu, reservation: Aeu) -> bool {
    spent.saturating_add(reservation) <= DAY_CAP
}

#[cfg(test)]
mod tests {
    use assay_domain::{Aeu, RequiredRole};

    use super::{DAY_CAP, SpendTier, fits, reservation};
    use crate::prompt_bundle::Role;
    use crate::shard::{MAX_SHARDS, ShardCount, full_run_cap};

    const FULL_ROLES: [RequiredRole; 4] = [
        RequiredRole::Bugs,
        RequiredRole::Security,
        RequiredRole::Design,
        RequiredRole::Meta,
    ];

    #[test]
    fn a_full_run_reserves_the_run_cap_of_the_most_shards() {
        let shards = ShardCount::new(MAX_SHARDS).expect("the most shards is a shard count");

        let reserved = reservation(SpendTier::Full, &FULL_ROLES, None);

        assert_eq!(reserved.total, full_run_cap(&FULL_ROLES, shards));
    }

    #[test]
    fn a_lite_run_reserves_its_run_cap() {
        let reserved = reservation(SpendTier::Lite, &[RequiredRole::Combined], None);

        assert_eq!(reserved.total, Aeu::from_micros(740_000));
    }

    #[test]
    fn a_micro_run_reserves_its_one_role() {
        let reserved = reservation(SpendTier::Micro, &[RequiredRole::CombinedMicro], None);

        assert_eq!(reserved.total, Aeu::from_micros(250_000));
    }

    #[test]
    fn a_granted_role_reserves_half_its_cap_more() {
        let plain = reservation(SpendTier::Lite, &[RequiredRole::Combined], None);

        let granted = reservation(
            SpendTier::Lite,
            &[RequiredRole::Combined],
            Some(Role::Combined),
        );

        let half = Role::Combined.cap().micros() / 2;
        assert_eq!(granted.total, Aeu::from_micros(plain.total.micros() + half));
    }

    #[test]
    fn a_grant_for_a_role_the_run_does_not_have_raises_nothing() {
        let plain = reservation(SpendTier::Lite, &[RequiredRole::Combined], None);

        let granted = reservation(SpendTier::Lite, &[RequiredRole::Combined], Some(Role::Bugs));

        assert_eq!(granted, plain);
    }

    #[test]
    fn a_reservation_up_to_the_cap_fits_and_one_past_it_does_not() {
        let spent = Aeu::from_micros(DAY_CAP.micros() - 740_000);

        let at_cap = fits(spent, Aeu::from_micros(740_000));
        let past_cap = fits(spent, Aeu::from_micros(740_001));

        assert_eq!((at_cap, past_cap), (true, false));
    }
}
