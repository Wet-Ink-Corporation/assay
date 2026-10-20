//! The `LedgerRefs` port: refs of the ledger repository, created only when absent and deleted
//! only when unchanged. Implemented by the forge adapter (#71) with the `assay-ledger-writer`
//! App's token (ADR-26).

use super::name::LeaseRef;
use crate::config::CommitId;

/// The longest lease message read or written, in bytes. A longer one is refused unparsed
/// (H-13); a lease document is about 200 bytes.
pub const MAX_LEASE_MESSAGE_BYTES: usize = 1024;

/// The ledger repository's lease refs.
///
/// Every message read back is untrusted (T23): the lease operations parse it strictly.
pub trait LedgerRefs {
    /// Why the ledger repository's refs could not be read or written.
    type Error: std::error::Error;

    /// Creates the ref `name` at a new commit whose message is `message`, only when the ref does
    /// not exist; creation is atomic, so of two creators exactly one sees
    /// [`RefCreation::Created`].
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the ref cannot be created and does not exist, or the outcome cannot
    /// be told.
    fn create(&self, name: &LeaseRef, message: &str) -> Result<RefCreation, Self::Error>;

    /// The commit `name` points at and that commit's message; `None` when the ref does not
    /// exist.
    ///
    /// An implementation must refuse a message over [`MAX_LEASE_MESSAGE_BYTES`].
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the ref cannot be read, or its message is over the cap.
    fn read(&self, name: &LeaseRef) -> Result<Option<RefTip>, Self::Error>;

    /// Deletes `name` only when it still points at `expected`, atomically; otherwise nothing
    /// changes and the outcome is [`RefDeletion::Moved`] (also when the ref is gone).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the deletion could not be made or refused; the ref may or may not
    /// have been deleted.
    fn delete_if(&self, name: &LeaseRef, expected: &CommitId) -> Result<RefDeletion, Self::Error>;
}

/// What [`LedgerRefs::create`] did.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RefCreation {
    /// The ref now points at this new commit.
    Created(CommitId),
    /// The ref already existed; nothing was changed.
    Exists,
}

/// A ref as read: the commit it points at and that commit's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTip {
    /// What a deletion is conditioned on.
    pub commit: CommitId,
    /// The commit's message, untrusted.
    pub message: String,
}

/// What [`LedgerRefs::delete_if`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefDeletion {
    /// The ref pointed at the expected commit and is gone.
    Deleted,
    /// The ref pointed elsewhere, or did not exist; nothing was deleted.
    Moved,
}
