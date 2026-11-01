//! The PR's previous completed run, as `admit` and `decide` both read it (04 §7, D6 A): the
//! ledger's state of the PR, and whether the previous run's open findings carry, decided on the
//! evidence pointer in that state. One reading, so the two jobs cannot decide the carry apart.

use std::error::Error;
use std::fmt;

use assay_app::{
    BundleSource, Carry, Ledger, LedgerReadError, PrState, PriorCarry, PriorCarryError,
    TargetError, TargetRequest, prior_head, read_state, review_target,
};
use assay_forge::git::GitError;
use assay_forge::process::ProcessError;

use crate::ci::RunEnv;
use crate::outputs::Identity;
use crate::pull_request::{ForgeObjectsError, ForgePullRequest};

/// The PR's ledger state, and the carry decided on its evidence pointer.
#[derive(Debug)]
pub(crate) struct Prior {
    /// Whether the previous completed run's open findings carry.
    pub(crate) carry: Carry,
    /// The PR's ledger state, whose evidence pointer the carry was decided on.
    pub(crate) state: PrState,
}

/// Why the previous run could not be read. `L` and `B` are the ledger's and the bundle store's
/// errors.
#[derive(Debug)]
pub(crate) enum PriorError<L, B> {
    /// The PR's ledger state could not be read.
    State(LedgerReadError<L>),
    /// The previous completed run's head could not be read from its bundle.
    PriorHead(PriorCarryError<B>),
    /// How the previous head stands to head could not be read.
    Target(TargetError<ForgeObjectsError>),
    /// The range-diff's git process group may still be running.
    RangeDiffKilled,
}

impl<L, B> fmt::Display for PriorError<L, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::State(_) => "could not read the PR's ledger state",
            Self::PriorHead(_) => "could not read the previous run's head",
            Self::Target(_) => "could not read how the previous head stands to head",
            Self::RangeDiffKilled => "the range-diff's git process group may still be running",
        })
    }
}

impl<L: Error + 'static, B: Error + 'static> Error for PriorError<L, B> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::State(source) => Some(source),
            Self::PriorHead(source) => Some(source),
            Self::Target(source) => Some(source),
            Self::RangeDiffKilled => None,
        }
    }
}

/// The PR's ledger state and whether the previous completed run's open findings carry: its
/// head is read from its bundle through the state's evidence pointer (D6 A), and
/// [`review_target`] decides.
///
/// The previous head is not fetched: when it is an ancestor of head, head's fetch brought it,
/// and otherwise the carry resets whether it is rewritten or unreachable.
pub(crate) fn prior<L: Ledger, B: BundleSource>(
    (run, identity): (&RunEnv, &Identity),
    objects: &ForgePullRequest<'_>,
    ledger: &L,
    bundles: &B,
) -> Result<Prior, PriorError<L::Error, B::Error>> {
    let state = read_state(ledger, run.run_uid.repo_id, identity.pr).map_err(PriorError::State)?;
    let previous = match &state.evidence {
        None => None,
        Some(pointer) => {
            let repository = run.repo.to_string();
            let prior = PriorCarry {
                repo_id: run.run_uid.repo_id,
                pointer,
                repository: &repository,
            };
            Some(prior_head(&prior, bundles).map_err(PriorError::PriorHead)?)
        }
    };
    let request = TargetRequest {
        base: &identity.base_sha,
        head: &identity.head_sha,
        previous: previous.as_ref(),
    };
    let target = review_target(&request, objects).map_err(PriorError::Target)?;
    if target.range_diff_failure().is_some_and(group_may_run) {
        return Err(PriorError::RangeDiffKilled);
    }
    Ok(Prior {
        carry: target.carry(),
        state,
    })
}

/// Whether `error` leaves a git process group that may still be running: the job must stop.
const fn group_may_run(error: &ForgeObjectsError) -> bool {
    matches!(
        error,
        ForgeObjectsError::Git(GitError::Process {
            source: ProcessError::Kill { .. },
            ..
        })
    )
}
