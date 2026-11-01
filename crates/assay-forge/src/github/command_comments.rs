//! `assay-app`'s [`CommandComments`] port: an issue comment re-read by id
//! (`GET issues/comments/{id}`), so the eligibility snapshot and `admit`'s dismissals can tell
//! whether the comment behind a command record still stands (05 T21, R3-02).

use assay_app::{CommandComments, CommentId, IssueComment, MAX_COMMENT_BYTES, Timestamp};
use serde::Deserialize;

use super::GitHub;
use super::dto::{self, UserDto, ViaApp};
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};

/// An issue comment as a command check reads it.
#[derive(Debug, Deserialize)]
struct CommandCommentDto {
    id: u64,
    user: Option<UserDto>,
    created_at: String,
    updated_at: String,
    body: Option<String>,
    #[serde(default, deserialize_with = "dto::via_app")]
    performed_via_github_app: ViaApp,
}

impl<T: Transport, S: Sleep> CommandComments for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    /// A 404 is a deleted comment, and is read without probing the repository: `None` voids
    /// the command, which only takes a privilege away, so a token that cannot see the
    /// repository never grants one.
    fn comment(&self, id: CommentId) -> Result<Option<IssueComment>, Self::Error> {
        let operation = Operation::ReadComment;
        let path = self.path(operation, &format!("issues/comments/{}", id.0))?;
        self.api
            .get_optional::<CommandCommentDto>(operation, &path)?
            .map(|comment| issue_comment(operation, id, comment))
            .transpose()
    }
}

/// `comment`, asked for as `id`.
fn issue_comment<E>(
    operation: Operation,
    id: CommentId,
    comment: CommandCommentDto,
) -> Result<IssueComment, ForgeError<E>> {
    let malformed = |detail| ForgeError::Malformed { operation, detail };
    if comment.id != id.0.get() {
        return Err(malformed("GitHub returned another comment"));
    }
    let body = comment
        .body
        .ok_or_else(|| malformed("a comment has no body"))?;
    if body.len() > MAX_COMMENT_BYTES {
        return Err(malformed("a comment body is over MAX_COMMENT_BYTES"));
    }
    let via_app = match comment.performed_via_github_app {
        ViaApp::Yes => true,
        ViaApp::No => false,
        ViaApp::Unstated => {
            return Err(malformed("a comment does not say whether an app posted it"));
        }
    };
    let time = |text: &str| {
        text.parse::<Timestamp>()
            .map_err(|_refused| malformed("a comment's time is not an RFC 3339 UTC date-time"))
    };
    Ok(IssueComment {
        id,
        user: dto::user(operation, comment.user.as_ref())?,
        created_at: time(&comment.created_at)?,
        updated_at: time(&comment.updated_at)?,
        body,
        via_app,
    })
}
