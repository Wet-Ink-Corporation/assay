//! `assay-app`'s `CommandReplies` port (#61 DO-9, D10): assay's closed reply to a command
//! comment, posted as the App.

use std::num::NonZeroU64;

use assay_app::{CommandReplies, CommentId, Reply};

use super::GitHub;
use super::error::ForgeError;
use super::transport::{Sleep, Transport};

impl<T: Transport, S: Sleep> CommandReplies for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    /// The body is [`Reply::body`], built by `assay-app`, so the comment's marker and nothing
    /// of the comment's text. A POST that failed retryably is sent again only when the App's
    /// comments do not already hold that body. Not made under the publish lock, so no deadline.
    fn reply(&self, pr: NonZeroU64, to: CommentId, reply: Reply) -> Result<(), Self::Error> {
        self.post_comment(pr, &reply.body(to), None)
    }
}
