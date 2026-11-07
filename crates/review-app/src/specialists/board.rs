//! Where workers post that their call has ended, and where the coordinator waits for that.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The slots whose calls have ended and not yet been collected.
///
/// A `Mutex` and a `Condvar` rather than a channel: a worker posts from a drop guard, so a
/// panicking runner still posts, and posting cannot fail (a channel's send can, and a `Drop`
/// could only discard that error). Every critical section is one `Vec` push or take, so a
/// poisoned lock holds a whole vector and is read through (`PoisonError::into_inner`).
#[derive(Debug, Default)]
pub(super) struct Board {
    ended: Mutex<Vec<usize>>,
    /// Paired with `ended` and no other mutex.
    posted: Condvar,
}

impl Board {
    fn ended(&self) -> MutexGuard<'_, Vec<usize>> {
        self.ended.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records that `slot`'s call ended and wakes the coordinator.
    fn post(&self, slot: usize) {
        self.ended().push(slot);
        self.posted.notify_one();
    }

    /// A guard that posts `slot` when dropped: when the worker returns or unwinds.
    pub(super) const fn poster(&self, slot: usize) -> Poster<'_> {
        Poster { board: self, slot }
    }

    /// The slots that ended, waiting up to `limit` for one when none has: empty only when the
    /// limit passed first.
    pub(super) fn take_within(&self, limit: Duration) -> Vec<usize> {
        let (mut ended, _timed_out) = self
            .posted
            .wait_timeout_while(self.ended(), limit, |ended| ended.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *ended)
    }

    /// The slots that ended, waiting for one when none has. Called only while a call is in
    /// flight, whose worker posts when it returns or unwinds; each call is bounded by its
    /// wall-clock limit, which the runner enforces.
    pub(super) fn take(&self) -> Vec<usize> {
        let mut ended = self
            .posted
            .wait_while(self.ended(), |ended| ended.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *ended)
    }
}

/// Posts its slot to the board when dropped.
#[derive(Debug)]
pub(super) struct Poster<'board> {
    board: &'board Board,
    slot: usize,
}

impl Drop for Poster<'_> {
    fn drop(&mut self) {
        self.board.post(self.slot);
    }
}
