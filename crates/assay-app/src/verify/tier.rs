//! The tiers the verifier serves, how many calls each allows and what they may spend (04 §3).

use assay_domain::{Aeu, Tier};

use super::VERIFIER_CAP;

/// The tiers with a verifier stage; `trivial` and `needs-split` have no LLM stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierTier {
    /// `full`: at most five candidates are verified, carried findings first.
    Full,
    /// `lite`: at most three candidates are verified, carried findings first.
    Lite,
    /// `micro`: no verifier; findings stay capped at `major` and advisory (04 §2.3).
    Micro,
}

impl VerifierTier {
    /// How many candidates the tier verifies, one call each (04 §3).
    #[must_use]
    pub const fn calls(self) -> usize {
        match self {
            Self::Full => 5,
            Self::Lite => 3,
            Self::Micro => 0,
        }
    }

    /// What the whole stage may spend, retries included: [`VERIFIER_CAP`] per call
    /// ($0.40 in `full`, $0.24 in `lite`, 04 §3).
    #[must_use]
    pub const fn budget(self) -> Aeu {
        let calls: u64 = match self {
            Self::Full => 5,
            Self::Lite => 3,
            Self::Micro => 0,
        };
        Aeu::from_micros(VERIFIER_CAP.micros().saturating_mul(calls))
    }
}

/// A tier with no verifier stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the {} tier has no verifier", .tier.code())]
pub struct UnsupportedTier {
    tier: Tier,
}

impl UnsupportedTier {
    /// The tier refused.
    #[must_use]
    pub const fn tier(self) -> Tier {
        self.tier
    }
}

impl TryFrom<Tier> for VerifierTier {
    type Error = UnsupportedTier;

    /// # Errors
    /// Returns [`UnsupportedTier`] for `trivial` and `needs-split`.
    fn try_from(tier: Tier) -> Result<Self, UnsupportedTier> {
        match tier {
            Tier::Full => Ok(Self::Full),
            Tier::Lite => Ok(Self::Lite),
            Tier::Micro => Ok(Self::Micro),
            Tier::Trivial | Tier::NeedsSplit => Err(UnsupportedTier { tier }),
        }
    }
}
