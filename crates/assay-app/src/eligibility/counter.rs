//! decide-only's per-actor command counter (D6 A, T9, T12): every `/assay` line of an actor on a
//! PR is counted in the PR's state before any other write of the run (J7), with no window; an
//! actor past [`MAX_COMMANDS_PER_ACTOR`] is limited unless privileged.

use std::num::NonZeroU64;

use crate::ledger::{
    ActorCounter, Ledger, LedgerWriteError, MAX_STATE_ENTRIES, PrState, StateUpdate, update_state,
};
use crate::status::UserId;

/// The most commands an actor that is not [privileged](Exempt::Privileged) may issue on one PR.
pub const MAX_COMMANDS_PER_ACTOR: u32 = 20;

/// Whether the limit applies to the actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exempt {
    /// The actor is in base `.assay/owners`' `owners` or `approvers` at the base tip: counted,
    /// never limited.
    Privileged,
    /// Any other actor: limited past [`MAX_COMMANDS_PER_ACTOR`].
    No,
}

/// The command [`count_command`] counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandCount {
    /// The repository, by numeric id.
    pub repo_id: NonZeroU64,
    /// The PR the command was issued on.
    pub pr: NonZeroU64,
    /// Who issued it (`comment.user.id`).
    pub actor: UserId,
    /// Whether the limit applies to them.
    pub exempt: Exempt,
}

/// What [`count_command`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Counted {
    /// The command may go on.
    Within {
        /// The actor's count with this command, or 0 when the state had no room to count a new
        /// actor.
        count: u32,
    },
    /// The command is refused for the limit; it was still counted.
    Limited {
        /// The actor's count with this command, or 0 when the state had no room to count a new
        /// actor.
        count: u32,
    },
}

/// Counts one command and says whether it is within the limit.
///
/// The command is counted whatever the answer, so a limited actor's count keeps rising. An actor
/// the state has no counter for, when it already holds [`MAX_STATE_ENTRIES`] counters, is not
/// counted and nothing is written: `Limited { count: 0 }`, or `Within { count: 0 }` when
/// privileged. A count at `u32::MAX` stays there. On a compare-and-set conflict the fresh count is
/// incremented.
///
/// # Errors
///
/// [`LedgerWriteError`] when the state cannot be read or written, as [`update_state`] reports.
pub fn count_command<L: Ledger>(
    ledger: &L,
    command: CommandCount,
) -> Result<Counted, LedgerWriteError<L::Error>> {
    let CommandCount {
        repo_id,
        pr,
        actor,
        exempt,
    } = command;
    let state = match update_state(ledger, repo_id, pr, |state| counted(state, actor))? {
        StateUpdate::Updated(state) | StateUpdate::Unchanged(state) => state,
    };
    let count = state
        .actor_counters
        .iter()
        .find(|counter| counter.actor_id == actor)
        .map_or(0, |counter| counter.count);
    Ok(classify(count, exempt))
}

/// `state` with one more command of `actor`, or `None` when it has no room for a new actor.
fn counted(state: &PrState, actor: UserId) -> Option<PrState> {
    let known = state
        .actor_counters
        .iter()
        .any(|counter| counter.actor_id == actor);
    if !known && state.actor_counters.len() >= MAX_STATE_ENTRIES {
        return None;
    }
    // update_state shows a borrowed state and takes an owned next one.
    let mut next = state.clone();
    match next
        .actor_counters
        .iter_mut()
        .find(|counter| counter.actor_id == actor)
    {
        Some(counter) => counter.count = counter.count.saturating_add(1),
        None => next.actor_counters.push(ActorCounter {
            actor_id: actor,
            count: 1,
        }),
    }
    Some(next)
}

/// The answer for an actor whose count is `count` (0: not counted for want of room).
fn classify(count: u32, exempt: Exempt) -> Counted {
    match exempt {
        Exempt::No if count == 0 || count > MAX_COMMANDS_PER_ACTOR => Counted::Limited { count },
        Exempt::Privileged | Exempt::No => Counted::Within { count },
    }
}
