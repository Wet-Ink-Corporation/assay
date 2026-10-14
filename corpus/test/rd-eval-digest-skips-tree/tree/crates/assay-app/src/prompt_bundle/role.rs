//! The reviewer roles with a v1 instruction, and their limits (04 §3 roster).

use std::num::NonZeroU8;

use assay_domain::{Aeu, RequiredRole, Severity};

/// A reviewer role that has an instruction in the bundle (04 §3). The coordinator and the
/// verifier are not roles: they write groups and verdicts, never findings of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// `bugs`: logic, edge cases, error handling, concurrency and resource bounds, for `full`.
    Bugs,
    /// `security`: injection, authorisation, secrets, deserialisation, dependencies and
    /// `build.rs`, for `full`.
    Security,
    /// `design`: API shape, layering, test adequacy and docs freshness, for `full`.
    Design,
    /// `meta`: the authority a trust-path or exec-surface change grants, for `full` on those
    /// classes only.
    Meta,
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
    pub const ALL: [Self; 6] = [
        Self::Bugs,
        Self::Security,
        Self::Design,
        Self::Meta,
        Self::Combined,
        Self::CombinedMicro,
    ];

    /// The role's name in the roster, in findings' `source.role` and in events: the code of
    /// the same [`RequiredRole`].
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Bugs => "bugs",
            Self::Security => "security",
            Self::Design => "design",
            Self::Meta => "meta",
            Self::Combined => "combined",
            Self::CombinedMicro => "combined-micro",
        }
    }

    /// The CLI's turn limit for the role (`ModelCall::max_turns`).
    #[must_use]
    pub const fn max_turns(self) -> NonZeroU8 {
        match self {
            Self::Bugs => turns(25),
            Self::Security | Self::Design | Self::Combined => turns(20),
            Self::Meta => turns(15),
            Self::CombinedMicro => turns(8),
        }
    }

    /// The role's cap (`ModelCall::budget`), provisional until calibration (04 §3, `A-21`).
    #[must_use]
    pub const fn cap(self) -> Aeu {
        match self {
            Self::Bugs => Aeu::from_micros(600_000),
            Self::Security | Self::Design | Self::Combined => Aeu::from_micros(500_000),
            Self::Meta => Aeu::from_micros(300_000),
            Self::CombinedMicro => Aeu::from_micros(250_000),
        }
    }

    /// The highest severity the role's findings may carry: `combined-micro`'s findings are
    /// capped at `major` (04 §2.3). The instruction says so; validation enforces it.
    #[must_use]
    pub const fn severity_ceiling(self) -> Severity {
        match self {
            Self::CombinedMicro => Severity::Major,
            Self::Bugs | Self::Security | Self::Design | Self::Meta | Self::Combined => {
                Severity::Blocker
            }
        }
    }

    /// Whether the role is sent the pull request's intent text (title, body, commit messages)
    /// inside its untrusted envelope. Only `design` and `meta` are; every other role is blind
    /// to it, so an author's "this is intentional" cannot anchor it (04 §3 "Blindness").
    #[must_use]
    pub const fn sees_intent(self) -> bool {
        match self {
            Self::Design | Self::Meta => true,
            Self::Bugs | Self::Security | Self::Combined | Self::CombinedMicro => false,
        }
    }
}

/// The bundle role the plan's required role runs as: the two enums list the same six roles.
impl From<RequiredRole> for Role {
    fn from(role: RequiredRole) -> Self {
        match role {
            RequiredRole::Bugs => Self::Bugs,
            RequiredRole::Security => Self::Security,
            RequiredRole::Design => Self::Design,
            RequiredRole::Meta => Self::Meta,
            RequiredRole::Combined => Self::Combined,
            RequiredRole::CombinedMicro => Self::CombinedMicro,
        }
    }
}

/// The required role a bundle role fulfils, as `decide` counts it (`completed_roles`).
impl From<Role> for RequiredRole {
    fn from(role: Role) -> Self {
        match role {
            Role::Bugs => Self::Bugs,
            Role::Security => Self::Security,
            Role::Design => Self::Design,
            Role::Meta => Self::Meta,
            Role::Combined => Self::Combined,
            Role::CombinedMicro => Self::CombinedMicro,
        }
    }
}
