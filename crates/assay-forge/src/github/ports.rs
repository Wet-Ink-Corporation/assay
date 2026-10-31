//! `assay-app`'s publication ports and its full `Forge` port, over the adapter's calls. The
//! publication calls `decide` makes under the publish lock carry its lease's deadline (#290,
//! #318 F6, #332), which every request and retry wait they make must end before.

use std::num::NonZeroU64;

use assay_app::{
    CheckFound, CheckName, CheckRun, CheckRunId, CommentForge, CommentId, CommitId, CommitIdentity,
    CompareAndSetError, Deadline, Forge, InlineComment, LockedWrite, Reaction, ReactionSubject,
    Review, SummaryComment, TimelineEvent, Upserted, VerdictForge,
};

use super::GitHub;
use super::error::ForgeError;
use super::transport::{Sleep, Transport};

impl<T: Transport, S: Sleep> VerdictForge for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn find_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        deadline: Option<Deadline>,
    ) -> Result<Option<CheckRun>, Self::Error> {
        self.find_app_check(head, name, deadline)
    }

    fn update_check(&self, id: CheckRunId, write: LockedWrite<'_>) -> Result<(), Self::Error> {
        self.patch_check(id, write.update, Some(write.deadline))
    }

    fn find_summary(
        &self,
        number: NonZeroU64,
        marker: &str,
        deadline: Deadline,
    ) -> Result<Option<SummaryComment>, Self::Error> {
        self.find_comment(number, marker, Some(deadline))
    }

    fn create_summary(
        &self,
        number: NonZeroU64,
        body: &str,
        deadline: Deadline,
    ) -> Result<(), Self::Error> {
        self.post_comment(number, body, Some(deadline))
    }

    fn update_summary(
        &self,
        id: CommentId,
        body: &str,
        deadline: Deadline,
    ) -> Result<(), Self::Error> {
        self.edit_comment(id, body, Some(deadline))
    }
}

impl<T: Transport, S: Sleep> CommentForge for GitHub<T, S> {
    fn app_review_comments(
        &self,
        number: NonZeroU64,
        deadline: Deadline,
    ) -> Result<Vec<String>, Self::Error> {
        self.app_review_bodies(number, Some(deadline))
    }

    fn create_review_comment(
        &self,
        number: NonZeroU64,
        comment: &InlineComment<'_>,
        deadline: Deadline,
    ) -> Result<(), Self::Error> {
        self.post_review_comment(number, comment, Some(deadline))
    }

    fn create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        write: LockedWrite<'_>,
    ) -> Result<(), Self::Error> {
        GitHub::create_check(self, head, name, write)
    }
}

impl<T: Transport, S: Sleep> Forge for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn find_or_create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: LockedWrite<'_>,
    ) -> Result<CheckFound, Self::Error> {
        self.find_or_create(head, name, initial)
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        write: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<Self::Error>> {
        self.compare_and_set(id, expected, write)
    }

    fn upsert_comment(
        &self,
        number: NonZeroU64,
        marker: &str,
        body: &str,
    ) -> Result<Upserted, Self::Error> {
        self.upsert(number, marker, body)
    }

    fn commit_identities(&self, commits: &[CommitId]) -> Result<Vec<CommitIdentity>, Self::Error> {
        self.resolve_commits(commits)
    }

    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, Self::Error> {
        self.list_reviews(number)
    }

    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, Self::Error> {
        self.list_labels(number)
    }

    fn reactions(&self, subject: ReactionSubject) -> Result<Vec<Reaction>, Self::Error> {
        self.list_reactions(subject)
    }

    fn timeline(&self, number: NonZeroU64) -> Result<Vec<TimelineEvent>, Self::Error> {
        self.read_timeline(number)
    }
}
