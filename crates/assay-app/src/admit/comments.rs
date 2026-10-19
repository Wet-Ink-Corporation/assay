//! The `CommandComments` port: the issue comments command records came from, as GitHub shows
//! them now, read only to void a record (05 T21, R3-02). The record, never the comment, is what
//! a dismissal says; the comment only shows whether it was edited or deleted since.

use crate::ledger::LedgerTime;
use crate::publish::CommentId;

/// The longest comment body read, in bytes: GitHub caps a body at 65,536 characters, at most
/// four bytes each.
pub const MAX_COMMENT_BYTES: usize = 262_144;

/// The PR's issue comments. Implemented by the forge adapter with the live GitHub transport
/// (#273); everything read is untrusted.
pub trait CommandComments {
    /// Why the comments could not be read.
    type Error: std::error::Error;

    /// The comment `id` as GitHub shows it now; `None` when it was deleted.
    ///
    /// An implementation must refuse a body over [`MAX_COMMENT_BYTES`].
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the comment cannot be read, or its body is over the cap.
    fn comment(&self, id: CommentId) -> Result<Option<CommentNow>, Self::Error>;
}

/// An issue comment as GitHub shows it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentNow {
    /// Its `created_at`.
    pub created_at: LedgerTime,
    /// Its `updated_at`: later than `created_at` once the comment was edited.
    pub updated_at: LedgerTime,
    /// Its body, untrusted: only its sha256 is compared.
    pub body: String,
}
