//! Comments: the App's summary comment found by its hidden marker and written only when it
//! changed, and the App's inline review comments (02 §5, 04 §6).

use std::num::NonZeroU64;

use assay_app::{CommentId, InlineComment, SummaryComment, Upserted};
use serde_json::{Map, Value};

use super::GitHub;
use super::api::{PER_PAGE, encode};
use super::dto::{self, IssueCommentDto, ReviewCommentDto, UserDto};
use super::error::{ForgeError, Operation};
use super::transport::{Request, Sleep, Transport};

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// Every issue comment on PR `number`, oldest first.
    fn issue_comments(
        &self,
        number: NonZeroU64,
    ) -> Result<Vec<IssueCommentDto>, ForgeError<T::Error>> {
        let path = self.path(&format!("issues/{number}/comments?per_page={PER_PAGE}"));
        self.api
            .list::<Vec<IssueCommentDto>>(Operation::ListComments, &path, None)
    }

    /// Whether the App authored `comment`: through the App, as its `[bot]` user.
    fn is_app_comment(&self, comment: &IssueCommentDto) -> bool {
        comment
            .performed_via_github_app
            .as_ref()
            .is_some_and(|app| app.id == self.identity.app.0.get())
            && comment.user.as_ref().is_some_and(|user| self.is_bot(user))
    }

    /// Whether `user` is the App's `[bot]` user.
    pub(crate) fn is_bot(&self, user: &UserDto) -> bool {
        user.id == self.identity.bot.0.get() && user.kind == "Bot"
    }

    /// The App's comment on PR `number` whose body contains `marker`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Ambiguous`] when the App has two; the listing's errors.
    pub(crate) fn find_comment(
        &self,
        number: NonZeroU64,
        marker: &str,
    ) -> Result<Option<SummaryComment>, ForgeError<T::Error>> {
        let operation = Operation::ListComments;
        let mut found = None;
        for comment in self.issue_comments(number)? {
            if !self.is_app_comment(&comment) {
                continue;
            }
            let Some(body) = comment.body.filter(|body| body.contains(marker)) else {
                continue;
            };
            if found.is_some() {
                return Err(ForgeError::Ambiguous { operation });
            }
            found = Some(SummaryComment {
                id: CommentId(dto::nonzero(operation, comment.id, "a comment id is zero")?),
                body,
            });
        }
        Ok(found)
    }

    /// Posts `body` on PR `number`. Before a retry the App's comments are read again, and a
    /// comment of exactly `body` means the earlier POST landed.
    pub(crate) fn post_comment(
        &self,
        number: NonZeroU64,
        body: &str,
    ) -> Result<(), ForgeError<T::Error>> {
        let operation = Operation::CreateComment;
        let payload = encode(operation, &body_only(body))?;
        let path = self.path(&format!("issues/{number}/comments"));
        self.api
            .post_once(operation, Request::post(&path, &payload), || {
                Ok(self.issue_comments(number)?.iter().any(|comment| {
                    self.is_app_comment(comment) && comment.body.as_deref() == Some(body)
                }))
            })
    }

    /// Replaces comment `id`'s body with `body`.
    pub(crate) fn edit_comment(
        &self,
        id: CommentId,
        body: &str,
    ) -> Result<(), ForgeError<T::Error>> {
        let operation = Operation::UpdateComment;
        let payload = encode(operation, &body_only(body))?;
        let path = self.path(&format!("issues/comments/{}", id.0));
        self.api
            .send(operation, Request::patch(&path, &payload))
            .map(drop)
    }

    /// The App's comment with `marker` made to read `body`, writing only what changed.
    pub(crate) fn upsert(
        &self,
        number: NonZeroU64,
        marker: &str,
        body: &str,
    ) -> Result<Upserted, ForgeError<T::Error>> {
        match self.find_comment(number, marker)? {
            None => self.post_comment(number, body).map(|()| Upserted::Created),
            Some(existing) if existing.body == body => Ok(Upserted::Unchanged),
            Some(existing) => self
                .edit_comment(existing.id, body)
                .map(|()| Upserted::Updated),
        }
    }

    /// Every review comment on PR `number`, oldest first.
    pub(crate) fn review_comments_of(
        &self,
        number: NonZeroU64,
    ) -> Result<Vec<ReviewCommentDto>, ForgeError<T::Error>> {
        let path = self.path(&format!("pulls/{number}/comments?per_page={PER_PAGE}"));
        self.api
            .list::<Vec<ReviewCommentDto>>(Operation::ListReviewComments, &path, None)
    }

    /// The bodies of the App's review comments on PR `number`.
    pub(crate) fn app_review_bodies(
        &self,
        number: NonZeroU64,
    ) -> Result<Vec<String>, ForgeError<T::Error>> {
        Ok(self
            .review_comments_of(number)?
            .into_iter()
            .filter(|comment| comment.user.as_ref().is_some_and(|user| self.is_bot(user)))
            .map(|comment| comment.body)
            .collect())
    }

    /// Posts `comment` on PR `number`'s diff. Before a retry the App's review comments are read
    /// again, and one of exactly its body means the earlier POST landed.
    pub(crate) fn post_review_comment(
        &self,
        number: NonZeroU64,
        comment: &InlineComment<'_>,
    ) -> Result<(), ForgeError<T::Error>> {
        let operation = Operation::CreateReviewComment;
        let payload = encode(operation, &review_comment_body(comment))?;
        let path = self.path(&format!("pulls/{number}/comments"));
        self.api
            .post_once(operation, Request::post(&path, &payload), || {
                Ok(self.app_review_bodies(number)?.contains(&comment.body))
            })
    }
}

/// `{"body": body}`.
fn body_only(body: &str) -> Value {
    let mut map = Map::new();
    map.insert("body".to_owned(), Value::from(body));
    Value::Object(map)
}

/// The JSON of a single review comment: `start_line` and `start_side` only for a span.
fn review_comment_body(comment: &InlineComment<'_>) -> Value {
    let side = comment.side.code();
    let mut map = Map::new();
    map.insert("body".to_owned(), Value::from(comment.body.as_str()));
    map.insert(
        "commit_id".to_owned(),
        Value::from(comment.commit_id.as_str()),
    );
    map.insert("path".to_owned(), Value::from(comment.path));
    map.insert("line".to_owned(), Value::from(comment.line.get()));
    map.insert("side".to_owned(), Value::from(side));
    if let Some(start) = comment.start_line {
        map.insert("start_line".to_owned(), Value::from(start.get()));
        map.insert("start_side".to_owned(), Value::from(side));
    }
    Value::Object(map)
}
