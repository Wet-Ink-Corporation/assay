//! The verdict check under the publish lock: found or created, then stamped, reopened or
//! published as `failure (re-deciding)` (02 §5, 06 §4.1, A-30).

use assay_domain::ReasonCode;

use super::{OpenClock, OpenError, OpenPorts, OpenStep, PendingStamp, ReopenRefused, Stamped};
use crate::config::CommitId;
use crate::forge::{CheckFound, CompareAndSetError, Forge};
use crate::ledger::{LeaseError, LockHold};
use crate::publish::{
    CheckConclusion, CheckName, CheckRun, CheckRunId, CheckState, CheckStatus, CheckUpdate, Unlock,
};

/// The check `open` writes and the lock it holds while writing it.
pub(super) struct Target<'a> {
    /// The PR's head.
    pub(super) head: &'a CommitId,
    /// The check's name.
    pub(super) name: &'a CheckName,
    /// This run's stamp.
    pub(super) stamp: PendingStamp,
    /// The publish lock held.
    pub(super) hold: &'a LockHold,
}

/// Why the stamp stopped under the lock, before the lock's fate is known.
pub(super) enum Halt<F, C> {
    /// The lease ran out (or the clock could not tell) before a write.
    Expired,
    /// The clock could not be read.
    Clock(C),
    /// A forge call failed.
    Forge(OpenStep, F),
    /// The check's stamp moved under the lock.
    Moved(String),
}

impl<F, C> Halt<F, C> {
    /// The run's error, once `unlock` says what became of the lock.
    pub(super) fn into_error<L, R>(self, unlock: Unlock<LeaseError<R>>) -> OpenError<F, L, R, C> {
        match self {
            Self::Expired => OpenError::LockExpired { unlock },
            Self::Clock(source) => OpenError::Clock { source, unlock },
            Self::Forge(step, source) => OpenError::Forge {
                step,
                source,
                unlock,
            },
            Self::Moved(found) => OpenError::Moved { found, unlock },
        }
    }
}

/// The [`Halt`] over the ports' error types.
type Halted<F, C> = Halt<<F as Forge>::Error, <C as OpenClock>::Error>;

/// What [`stamp`] returns over the ports' error types.
type Stamp<F, C> = Result<(CheckRunId, Stamped<<F as Forge>::Error>), Halted<F, C>>;

/// Finds or creates the check and stamps it for this run, every write made only while the
/// lock's lease is still live.
pub(super) fn stamp<F: Forge, L, R, C: OpenClock>(
    target: &Target<'_>,
    ports: &OpenPorts<'_, F, L, R, C>,
) -> Stamp<F, C> {
    let forge = ports.forge;
    let pending = pending_update(target.stamp);
    held(target, ports.clock)?;
    let current = match forge.find_or_create_check(target.head, target.name, &pending) {
        Ok(CheckFound::Created(created)) => return Ok((created.id, Stamped::Created)),
        Ok(CheckFound::Found(found)) => found,
        Err(error) => return Err(Halt::Forge(OpenStep::FindOrCreate, error)),
    };
    let id = current.id;
    if let Some(theirs) = PendingStamp::parse(&current.external_id)
        && theirs.run_id() > target.stamp.run_id()
    {
        return Ok((
            id,
            Stamped::Yielded {
                found: current.external_id,
            },
        ));
    }
    if current.status == CheckStatus::InProgress && current.external_id == pending.external_id {
        return Ok((id, Stamped::Unchanged));
    }
    held(target, ports.clock)?;
    let written = forge.update_check_if(id, &current.external_id, &pending);
    if current.status != CheckStatus::Completed {
        return written
            .map(|()| (id, Stamped::Restamped))
            .map_err(|error| cas_halt(OpenStep::Restamp, error));
    }
    reopen(target, ports, written).map(|stamped| (id, stamped))
}

/// The outcome of reopening a completed check: confirmed by reading it back, or published as
/// `failure (re-deciding)` when GitHub refused the write or kept the check completed.
fn reopen<F: Forge, L, R, C: OpenClock>(
    target: &Target<'_>,
    ports: &OpenPorts<'_, F, L, R, C>,
    written: Result<(), CompareAndSetError<F::Error>>,
) -> Result<Stamped<F::Error>, Halted<F, C>> {
    let pending = pending_update(target.stamp);
    let refused = match written {
        Ok(()) => ReopenRefused::KeptConclusion,
        Err(CompareAndSetError::Conflict { found }) => return Err(Halt::Moved(found)),
        Err(CompareAndSetError::Forge(error)) => ReopenRefused::Rejected(error),
    };
    let reread = reread(target, ports.forge, &pending)?;
    if matches!(refused, ReopenRefused::KeptConclusion)
        && reread.status == CheckStatus::InProgress
        && reread.external_id == pending.external_id
    {
        return Ok(Stamped::Reopened);
    }
    held(target, ports.clock)?;
    ports
        .forge
        .update_check_if(reread.id, &reread.external_id, &re_deciding(target.stamp))
        .map_err(|error| cas_halt(OpenStep::ReDeciding, error))?;
    Ok(Stamped::ReDeciding(refused))
}

/// The check as GitHub has it now. It exists, so nothing is created; were it gone, the one
/// created would carry this run's stamp `in_progress`.
fn reread<F: Forge, C>(
    target: &Target<'_>,
    forge: &F,
    pending: &CheckUpdate,
) -> Result<CheckRun, Halt<F::Error, C>> {
    match forge.find_or_create_check(target.head, target.name, pending) {
        Ok(CheckFound::Found(check) | CheckFound::Created(check)) => Ok(check),
        Err(error) => Err(Halt::Forge(OpenStep::Reread, error)),
    }
}

/// A failed compare-and-set as the halt it is.
fn cas_halt<F, C>(step: OpenStep, error: CompareAndSetError<F>) -> Halt<F, C> {
    match error {
        CompareAndSetError::Conflict { found } => Halt::Moved(found),
        CompareAndSetError::Forge(error) => Halt::Forge(step, error),
    }
}

/// Whether the lock's lease is still live now; a write is made only then (R3-09).
fn held<F, C: OpenClock>(target: &Target<'_>, clock: &C) -> Result<(), Halt<F, C::Error>> {
    let now = clock.now().map_err(Halt::Clock)?;
    if now < target.hold.lease.expires_at {
        Ok(())
    } else {
        Err(Halt::Expired)
    }
}

/// The check as `open` stamps it: `in_progress`, this run's stamp. No `started_at`: an existing
/// check keeps its own (R3-07).
fn pending_update(stamp: PendingStamp) -> CheckUpdate {
    CheckUpdate {
        state: CheckState::InProgress,
        title: "assay: pending".to_owned(),
        summary: format!(
            "assay run {} is deciding this head; the verdict follows.",
            stamp.run_id()
        ),
        text: String::new(),
        external_id: stamp.to_string(),
    }
}

/// The fail-safe state of a check that could not be reopened (A-30): completed `failure` with
/// reason `re-deciding`, carrying this run's stamp so its `decide` publishes over it.
fn re_deciding(stamp: PendingStamp) -> CheckUpdate {
    let code = ReasonCode::ReDeciding.code();
    CheckUpdate {
        state: CheckState::Completed(CheckConclusion::Failure),
        title: format!("assay: failure ({code})"),
        summary: format!(
            "The earlier verdict could not be reopened, so it fails ({code}) until assay run {} \
             decides this head again.",
            stamp.run_id()
        ),
        text: String::new(),
        external_id: stamp.to_string(),
    }
}
