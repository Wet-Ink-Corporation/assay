//! A read several checks share, made once per run (#75 review round 1, F7).

use std::cell::Cell;

use super::report::PrerequisiteFailure;

/// The outcome of one read that several checks judge.
///
/// A forge error is not `Clone`, so a failed read cannot fail every check with its own copy of
/// the cause: the first check to ask gets the cause, and every later one
/// [`PrerequisiteFailure::AlreadyReported`]. The `Cell` is that one hand-over; the checks run
/// one after another on one thread, and `Scene` is not shared beyond a call to
/// `prerequisites`.
pub(super) struct Shared<T, E> {
    /// What was read, for the later failures.
    what: &'static str,
    read: Result<T, Cell<Option<E>>>,
}

impl<T, E> Shared<T, E> {
    /// Keeps `read`, the outcome of reading `what`.
    pub(super) fn new(what: &'static str, read: Result<T, E>) -> Self {
        Self {
            what,
            read: read.map_err(|cause| Cell::new(Some(cause))),
        }
    }

    /// What was read; on a failed read, the cause for the first caller and
    /// [`PrerequisiteFailure::AlreadyReported`] for every later one.
    pub(super) fn get(&self) -> Result<&T, PrerequisiteFailure<E>> {
        match &self.read {
            Ok(value) => Ok(value),
            Err(cause) => Err(cause.take().map_or(
                PrerequisiteFailure::AlreadyReported { what: self.what },
                PrerequisiteFailure::Unreadable,
            )),
        }
    }
}
