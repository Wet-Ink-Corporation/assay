//! The tiers the verifier serves and how many calls each allows (04 §3).

use assay_domain::Tier;

/// The tiers whose verifier this story implements. `full` (<= 5 calls, carried findings
/// first) is #97's; `trivial` and `needs-split` have no LLM stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierTier {
    /// `lite`: at most three candidates are verified.
    Lite,
    /// `micro`: no verifier; findings stay capped at `major` and advisory (04 §2.3).
    Micro,
}

impl VerifierTier {
    /// How many candidates the tier verifies, one call each (04 §3).
    #[must_use]
    pub const fn calls(self) -> usize {
        match self {
            Self::Lite => 3,
            Self::Micro => 0,
        }
    }
}

/// A tier this verifier does not serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the {} tier has no verifier here", .tier.code())]
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
    /// Returns [`UnsupportedTier`] for `full`, `trivial` and `needs-split`.
    fn try_from(tier: Tier) -> Result<Self, UnsupportedTier> {
        match tier {
            Tier::Lite => Ok(Self::Lite),
            Tier::Micro => Ok(Self::Micro),
            Tier::Full | Tier::Trivial | Tier::NeedsSplit => Err(UnsupportedTier { tier }),
        }
    }
}
