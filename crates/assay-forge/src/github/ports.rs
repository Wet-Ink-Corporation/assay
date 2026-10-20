//! `assay-app`'s publication ports and its full `Forge` port, over the adapter's calls.

use std::num::NonZeroU64;

use assay_app::{
    CheckFound, CheckName, CheckRun, CheckRunId, CheckUpdate, CommentForge, CommentId, CommitId,
    CommitIdentity, CompareAndSetError, Forge, InlineComment, Reaction, ReactionSubject, Review,
    SummaryComment, TimelineEvent, Upserted, VerdictForge,
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
    ) -> Result<Option<CheckRun>, Self::Error> {
        self.find_app_check(head, name)
    }

    fn update_check(&self, id: CheckRunId, update: &CheckUpdate) -> Result<(), Self::Error> {
        self.patch_check(id, update)
    }

    fn find_summary(
        &self,
        number: NonZeroU64,
        marker: &str,
    ) -> Result<Option<SummaryComment>, Self::Error> {
        self.find_comment(number, marker)
    }

    fn create_summary(&self, number: NonZeroU64, body: &str) -> Result<(), Self::Error> {
        self.post_comment(number, body)
    }

    fn update_summary(&self, id: CommentId, body: &str) -> Result<(), Self::Error> {
        self.edit_comment(id, body)
    }
}

impl<T: Transport, S: Sleep> CommentForge for GitHub<T, S> {
    fn app_review_comments(&self, number: NonZeroU64) -> Result<Vec<String>, Self::Error> {
        self.app_review_bodies(number)
    }

    fn create_review_comment(
        &self,
        number: NonZeroU64,
        comment: &InlineComment<'_>,
    ) -> Result<(), Self::Error> {
        self.post_review_comment(number, comment)
    }

    fn create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        update: &CheckUpdate,
    ) -> Result<(), Self::Error> {
        GitHub::create_check(self, head, name, update)
    }
}

impl<T: Transport, S: Sleep> Forge for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn find_or_create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: &CheckUpdate,
    ) -> Result<CheckFound, Self::Error> {
        self.find_or_create(head, name, initial)
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        update: &CheckUpdate,
    ) -> Result<(), CompareAndSetError<Self::Error>> {
        self.compare_and_set(id, expected, update)
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
