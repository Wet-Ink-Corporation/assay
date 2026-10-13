//! What the specialists' fan-out produces: each role's outcome, attempts, cost, rate limits and
//! session init, in roster order.

use std::collections::BTreeSet;
use std::io;

use assay_domain::{Aeu, CallFailure, RequiredRole};

use crate::model_runner::{RateLimitObserved, Structured};
use crate::prompt_bundle::Role;
use crate::session_init::{InitReadError, SessionInit};

/// The outcome of the fan-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specialists<E> {
    /// One report per distinct role requested, in roster order.
    pub roles: Vec<RoleReport<E>>,
    /// The run cap the calls were held to: the sum of the roles' caps (06 §1).
    pub run_cap: Aeu,
    /// Why the stage stopped starting calls, if it did: the run cap, a pause for a rate limit
    /// or quota, or a runner's panic. A paused or stopped stage starts no later shard
    /// ([`run_shards`](crate::run_shards)).
    pub halt: Option<Halt>,
}

impl<E> Specialists<E> {
    /// What every attempt of every role cost, to ledger.
    #[must_use]
    pub fn cost(&self) -> Aeu {
        self.roles
            .iter()
            .fold(Aeu::ZERO, |total, report| total.saturating_add(report.cost))
    }

    /// The roles that produced structured output, for `decide`'s `completed_roles`. Whether the
    /// output is accepted is validation's, downstream.
    #[must_use]
    pub fn completed_roles(&self) -> BTreeSet<RequiredRole> {
        self.roles
            .iter()
            .filter(|report| matches!(report.outcome, RoleOutcome::Completed(_)))
            .map(|report| RequiredRole::from(report.role))
            .collect()
    }
}

/// What one role came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleReport<E> {
    /// The role.
    pub role: Role,
    /// How it ended.
    pub outcome: RoleOutcome<E>,
    /// Model calls made for it: 0 when skipped, 2 when a malformed answer was retried.
    pub attempts: u8,
    /// What its attempts cost: each one's reported cost, or its budget when the cost is unknown
    /// (a runner error or a panic after the model may have started).
    pub cost: Aeu,
    /// The rate-limit events of its attempts, in order.
    pub rate_limits: Vec<RateLimitObserved>,
    /// The session init of its last attempt that returned, as read. A call whose init is not
    /// clean must not be used (03 §8); judging it is the caller's.
    pub init: Option<Result<SessionInit, InitReadError>>,
}

/// How a role ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleOutcome<E> {
    /// The model produced structured output, unvalidated, with `source.role` set to the role
    /// that ran (#173): validation's role ceiling never trusts the model's own claim.
    Completed(Structured),
    /// The role was called and produced nothing usable.
    Failed(RoleFailure<E>),
    /// The role was never called: the stage stopped starting calls.
    Skipped(Halt),
}

/// Why a role that was called produced nothing usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleFailure<E> {
    /// The call ended without output: its own budget, turns or wall-clock limit, quota
    /// (`unavailable`), or malformed output, either after the one retry or when nothing was left
    /// of the run cap for that retry, or when the stage paused before making it.
    Call(CallFailure),
    /// The runner could not make or supervise the call; it is charged at its budget.
    Runner(E),
    /// The OS refused the thread for the call; nothing ran and nothing is charged.
    Spawn(io::ErrorKind),
    /// The runner panicked, a bug in the adapter; the call is charged at its budget and no
    /// further call starts.
    Panicked,
    /// The fan-out lost track of the call's thread, a bug in assay's bookkeeping that its
    /// construction rules out; the call is charged at its budget.
    Lost,
}

impl<E> RoleFailure<E> {
    /// The degraded reason `decide` reads (04 §5 rules 8b, 8c): an adapter failure means no
    /// model could be called.
    #[must_use]
    pub const fn reason(&self) -> CallFailure {
        match self {
            Self::Call(reason) => *reason,
            Self::Runner(_) | Self::Spawn(_) | Self::Panicked | Self::Lost => {
                CallFailure::Unavailable
            }
        }
    }
}

/// Why the stage stopped starting calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// The next call's cap no longer fitted under the run cap with nothing left in flight
    /// (06 §1: remaining roles are skipped when the run cap is reached).
    RunCap,
    /// A call reported quota exhausted or a `rejected` or `allowed_warning` rate limit: the
    /// stage pauses (06 §1).
    RateLimited,
    /// A runner panicked: nothing more is started (ASY-04).
    Panicked,
}
