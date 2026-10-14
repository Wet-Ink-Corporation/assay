//! The reviewer roles with a v1 instruction, and their limits (04 §3 roster).

use std::num::NonZeroU8;

use assay_domain::{Aeu, Severity};

/// A reviewer role that has an instruction in the bundle (04 §3). The full-tier roles, the
/// coordinator and the verifier join with the stories that write their instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// `combined`: bugs, security and design in one pass, for the `lite` tier.
    Combined,
    /// `combined-micro`: a short, focused pass on a tiny diff, for the `micro` tier.
    CombinedMicro,
}

/// `n` as a turn limit; only called with non-zero constants.
const fn turns(n: u8) -> NonZeroU8 {
    match NonZeroU8::new(n) {
        Some(turns) => turns,
        None => NonZeroU8::MIN,
    }
}

impl Role {
    /// Every role, in roster order.
    pub const ALL: [Self; 2] = [Self::Combined, Self::CombinedMicro];

    /// The role's name in the roster, in findings' `source.role` and in events.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Combined => "combined",
            Self::CombinedMicro => "combined-micro",
        }
    }

    /// The CLI's turn limit for the role (`ModelCall::max_turns`).
    #[must_use]
    pub const fn max_turns(self) -> NonZeroU8 {
        match self {
            Self::Combined => turns(20),
            Self::CombinedMicro => turns(8),
        }
    }

    /// The role's cap (`ModelCall::budget`), provisional until calibration (04 §3, `A-21`).
    #[must_use]
    pub const fn cap(self) -> Aeu {
        match self {
            Self::Combined => Aeu::from_micros(500_000),
            Self::CombinedMicro => Aeu::from_micros(250_000),
        }
    }

    /// The highest severity the role's findings may carry: `combined-micro`'s findings are
    /// capped at `major` (04 §2.3). The instruction says so; validation enforces it.
    #[must_use]
    pub const fn severity_ceiling(self) -> Severity {
        match self {
            Self::Combined => Severity::Blocker,
            Self::CombinedMicro => Severity::Major,
        }
    }
}
