//! A fake PR world for the eligibility rule: its git range, GitHub's reviews and commit
//! identities, the ledger's push and command records, and the comments behind the commands.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use assay_app::{
    Actor, CheckFound, CheckName, CheckRunId, CheckUpdate, CommandComments, CommentAction,
    CommentEvent, CommentId, CommitId, CommitIdentity, CompareAndSetError, EligibilityLists, Forge,
    GitHubState, IssueComment, LedgerRecords, LedgerTime, MergeTree, PushDraft, PushKind,
    PushRecord, RangeCommit, RangeListing, RangeObjects, Reaction, ReactionSubject, Review,
    ReviewState, Sha256Digest, SnapshotCommand, SnapshotLedger, SnapshotStep, StateError,
    StatePorts, StateRequest, TimelineEvent, Timestamp, TreeId, Upserted, UserId, github_state,
};
use assay_domain::{Fingerprint, RunUid};

/// The break-glass owner (in `owners`).
pub(crate) const OWNER: u64 = 1001;
/// The approver account (in `approvers`).
pub(crate) const APPROVER: u64 = 1002;
/// The owner's daily account (0005): a maintainer and the agent identity.
pub(crate) const DAILY: u64 = 124_330_161;
/// A contributor on no list.
pub(crate) const CONTRIBUTOR: u64 = 2000;
/// GitHub's `web-flow`, the committer of web merges.
pub(crate) const WEB_FLOW: u64 = 19_864_447;

pub(crate) const LISTS_OWNERS: &str = r#"
owners = [{ id = 1001, login = "break-glass" }]
approvers = [{ id = 1002, login = "approver" }]
maintainers = [{ id = 124330161, login = "Ryan-Britton" }]
"#;
pub(crate) const LISTS_AGENTS: &str = r#"agents = [{ id = 124330161, login = "Ryan-Britton" }]"#;

pub(crate) fn user(id: u64) -> UserId {
    UserId(NonZeroU64::new(id).expect("test ids are not zero"))
}

/// A commit id of forty copies of `digit`.
pub(crate) fn sha(digit: char) -> CommitId {
    digit
        .to_string()
        .repeat(40)
        .parse()
        .expect("forty hex digits are a commit id")
}

/// A tree id of forty copies of `digit`.
pub(crate) fn tree(digit: char) -> TreeId {
    digit
        .to_string()
        .repeat(40)
        .parse()
        .expect("forty hex digits are a tree id")
}

/// `2026-10-13T09:<minute>:00Z`.
pub(crate) fn at(minute: u32) -> Timestamp {
    format!("2026-10-13T09:{minute:02}:00Z")
        .parse()
        .expect("a fixed time parses")
}

pub(crate) fn lists() -> EligibilityLists {
    EligibilityLists::parse(Some(LISTS_OWNERS), Some(LISTS_AGENTS)).expect("the test lists parse")
}

/// The base commit and the base's tip.
pub(crate) const BASE: char = '0';
/// The PR's first commit.
pub(crate) const FIRST: char = '1';

/// Who made a commit, as GitHub resolves them: every author it lists (the git author first)
/// and the committer.
#[derive(Debug)]
pub(crate) struct By {
    pub(crate) authors: Vec<Actor>,
    pub(crate) committer: Actor,
}

impl By {
    /// Authored and committed by user `id`.
    pub(crate) fn user(id: u64) -> Self {
        Self::new(Actor::User(user(id)), Actor::User(user(id)))
    }

    /// Authored by `author` alone and committed by `committer`.
    pub(crate) fn new(author: Actor, committer: Actor) -> Self {
        Self {
            authors: vec![author],
            committer,
        }
    }
}

/// [`fp`]'s text.
pub(crate) const FP: &str = "0123456789abcdef";
/// [`digest`]'s text.
pub(crate) const DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// A fingerprint.
pub(crate) fn fp() -> Fingerprint {
    FP.parse().expect("sixteen hex digits are a fingerprint")
}

/// A reasons digest.
pub(crate) fn digest() -> Sha256Digest {
    DIGEST.parse().expect("a sha256 digest parses")
}

/// Comment `id` by `author`, created and last updated at `minute`.
pub(crate) fn comment(id: u64, author: u64, minute: u32, body: &str) -> IssueComment {
    IssueComment {
        id: CommentId(NonZeroU64::new(id).expect("comment ids are not zero")),
        user: Some(user(author)),
        created_at: at(minute),
        updated_at: at(minute),
        body: body.to_owned(),
        via_app: false,
    }
}

/// The `created` event of `comment`.
pub(crate) fn created(comment: IssueComment) -> CommentEvent {
    CommentEvent {
        action: CommentAction::Created,
        comment,
    }
}

/// [`at`]`(minute)` as the ledger writes it.
fn ledger_at(minute: u32) -> LedgerTime {
    format!("2026-10-13T09:{minute:02}:00.000Z")
        .parse()
        .expect("a fixed time parses")
}

/// Push record number `seq` of PR 7, its run's only one: `kind` onto `after`, `by` its sender
/// at a minute.
fn push_record(seq: usize, kind: PushKind, after: char, by: (u64, u32)) -> PushRecord {
    let (sender, minute) = by;
    let seq = NonZeroU64::new(u64::try_from(seq).expect("few pushes")).expect("seq starts at one");
    PushRecord {
        seq,
        push: PushDraft {
            run_uid: RunUid {
                repo_id: NonZeroU64::new(42).expect("forty-two is not zero"),
                run_id: seq,
                run_attempt: NonZeroU64::MIN,
            },
            pr: NonZeroU64::new(7).expect("seven is not zero"),
            kind,
            after: sha(after),
            sender_id: user(sender),
            at: ledger_at(minute),
        },
    }
}

/// Why a fake call failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FakeError {
    /// A call the eligibility rule never makes.
    #[error("not faked")]
    NotFaked,
    /// A commit the fake does not hold.
    #[error("no such object")]
    NoSuchObject,
    /// The call a test made fail.
    #[error("{0:?} failed")]
    Injected(SnapshotStep),
}

/// The PR as the fakes see it.
#[derive(Debug)]
pub(crate) struct World {
    pub(crate) number: NonZeroU64,
    pub(crate) author: UserId,
    pub(crate) head: CommitId,
    pub(crate) range: RangeListing,
    pub(crate) identities: BTreeMap<CommitId, CommitIdentity>,
    pub(crate) trees: BTreeMap<CommitId, TreeId>,
    pub(crate) merges: BTreeMap<(CommitId, CommitId), MergeTree>,
    /// `(ancestor, of)` pairs that hold.
    pub(crate) ancestry: BTreeSet<(CommitId, CommitId)>,
    pub(crate) pushes: Vec<PushRecord>,
    pub(crate) reviews: Vec<Review>,
    pub(crate) commands: Vec<SnapshotCommand>,
    pub(crate) comments: BTreeMap<CommentId, IssueComment>,
    pub(crate) lists: EligibilityLists,
    /// The port call that fails, if any.
    pub(crate) fail: Option<SnapshotStep>,
    /// What `commit_identities` answers instead of looking the commits up.
    pub(crate) identities_answer: Option<Vec<CommitIdentity>>,
    /// Whether the ledger lists every record whatever the cap (an adapter defect).
    pub(crate) overfull_ledger: bool,
    /// The caps the ledger was asked for, in call order.
    pub(crate) asked: RefCell<Vec<(SnapshotStep, usize)>>,
}

impl World {
    /// A one-commit PR by `author`, who authored and committed it and opened the PR at minute 0.
    pub(crate) fn opened_by(author: u64) -> Self {
        Self::opened_at(author, FIRST)
    }

    /// Like [`World::opened_by`], with commit `first` as the PR's one commit.
    pub(crate) fn opened_at(author: u64, first: char) -> Self {
        let mut world = Self {
            number: NonZeroU64::new(7).expect("seven is not zero"),
            author: user(author),
            head: sha(first),
            range: RangeListing::Listed(Vec::new()),
            identities: BTreeMap::new(),
            trees: BTreeMap::new(),
            merges: BTreeMap::new(),
            ancestry: BTreeSet::new(),
            pushes: Vec::new(),
            reviews: Vec::new(),
            commands: Vec::new(),
            comments: BTreeMap::new(),
            lists: lists(),
            fail: None,
            identities_answer: None,
            overfull_ledger: false,
            asked: RefCell::new(Vec::new()),
        };
        world.commit(first, &[BASE], By::user(author));
        world
            .pushes
            .push(push_record(1, PushKind::Opener, first, (author, 0)));
        world
    }

    /// Adds commit `id` with `parents` to the range (newest first), made `by` the users GitHub
    /// resolves.
    pub(crate) fn commit(&mut self, id: char, parents: &[char], by: By) {
        let By { authors, committer } = by;
        let commit = sha(id);
        if let RangeListing::Listed(commits) = &mut self.range {
            commits.insert(
                0,
                RangeCommit {
                    commit: commit.clone(),
                    parents: parents.iter().map(|&parent| sha(parent)).collect(),
                },
            );
        }
        self.identities.insert(
            commit.clone(),
            CommitIdentity {
                commit,
                authors,
                committer,
            },
        );
    }

    /// Makes `head` the PR's head.
    pub(crate) fn head(&mut self, head: char) {
        self.head = sha(head);
    }

    /// A push of `(before, after)` by `sender` at `minute`.
    pub(crate) fn push(&mut self, step: (char, char), sender: u64, minute: u32) {
        self.pushed(step, false, (sender, minute));
    }

    /// A force-push of `(before, after)` by `sender` at `minute`.
    pub(crate) fn force_push(&mut self, step: (char, char), sender: u64, minute: u32) {
        self.pushed(step, true, (sender, minute));
    }

    /// The PR's next push record: a push of `(before, after)`, `forced` or not, `by` its sender
    /// at a minute.
    fn pushed(&mut self, (before, after): (char, char), forced: bool, by: (u64, u32)) {
        let kind = PushKind::Push {
            before: sha(before),
            forced,
        };
        let seq = self.pushes.len() + 1;
        self.pushes.push(push_record(seq, kind, after, by));
    }

    /// A review by `reviewer` on `(commit, minute)`: of `commit`, submitted at `minute`; ids
    /// grow in call order.
    pub(crate) fn review(&mut self, reviewer: u64, state: ReviewState, on: (char, u32)) {
        let (commit, minute) = on;
        let id = u64::try_from(self.reviews.len()).expect("few reviews") + 1;
        self.reviews.push(Review {
            id: NonZeroU64::new(id).expect("ids start at one"),
            reviewer: Some(user(reviewer)),
            state,
            commit: Some(sha(commit)),
            submitted_at: Some(at(minute)),
        });
    }

    /// The PR's eligibility snapshot.
    pub(crate) fn state(
        &self,
    ) -> Result<GitHubState, StateError<FakeError, FakeError, FakeError, FakeError>> {
        let base = sha(BASE);
        github_state(
            &StatePorts {
                forge: self,
                objects: self,
                ledger: self,
                comments: self,
            },
            &StateRequest {
                number: self.number,
                author: self.author,
                base_sha: &base,
                head_sha: &self.head,
                lists: &self.lists,
            },
        )
    }
}

impl Forge for World {
    type Error = FakeError;

    fn find_or_create_check(
        &self,
        _head: &CommitId,
        _name: &CheckName,
        _initial: &CheckUpdate,
    ) -> Result<CheckFound, Self::Error> {
        Err(FakeError::NotFaked)
    }

    fn update_check_if(
        &self,
        _id: CheckRunId,
        _expected: &str,
        _update: &CheckUpdate,
    ) -> Result<(), CompareAndSetError<Self::Error>> {
        Err(CompareAndSetError::Forge(FakeError::NotFaked))
    }

    fn upsert_comment(
        &self,
        _number: NonZeroU64,
        _marker: &str,
        _body: &str,
    ) -> Result<Upserted, Self::Error> {
        Err(FakeError::NotFaked)
    }

    fn commit_identities(&self, commits: &[CommitId]) -> Result<Vec<CommitIdentity>, Self::Error> {
        self.failing(SnapshotStep::Identities)?;
        if let Some(answer) = &self.identities_answer {
            return Ok(answer.clone());
        }
        commits
            .iter()
            .map(|commit| {
                self.identities
                    .get(commit)
                    .cloned()
                    .ok_or(FakeError::NoSuchObject)
            })
            .collect()
    }

    fn reviews(&self, _number: NonZeroU64) -> Result<Vec<Review>, Self::Error> {
        self.failing(SnapshotStep::Reviews)?;
        Ok(self.reviews.clone())
    }

    fn labels(&self, _number: NonZeroU64) -> Result<Vec<String>, Self::Error> {
        Err(FakeError::NotFaked)
    }

    fn reactions(&self, _subject: ReactionSubject) -> Result<Vec<Reaction>, Self::Error> {
        Err(FakeError::NotFaked)
    }

    fn timeline(&self, _number: NonZeroU64) -> Result<Vec<TimelineEvent>, Self::Error> {
        Err(FakeError::NotFaked)
    }
}

impl RangeObjects for World {
    type Error = FakeError;

    fn range(
        &self,
        _base: &CommitId,
        _head: &CommitId,
        _max: usize,
    ) -> Result<RangeListing, Self::Error> {
        self.failing(SnapshotStep::RangeList)?;
        Ok(self.range.clone())
    }

    fn tree(&self, commit: &CommitId) -> Result<TreeId, Self::Error> {
        self.failing(SnapshotStep::MergeCheck)?;
        self.trees
            .get(commit)
            .cloned()
            .ok_or(FakeError::NoSuchObject)
    }

    fn merge_tree(&self, ours: &CommitId, theirs: &CommitId) -> Result<MergeTree, Self::Error> {
        self.failing(SnapshotStep::MergeCheck)?;
        self.merges
            .get(&(ours.clone(), theirs.clone()))
            .cloned()
            .ok_or(FakeError::NoSuchObject)
    }

    fn is_ancestor(&self, ancestor: &CommitId, of: &CommitId) -> Result<bool, Self::Error> {
        self.failing(SnapshotStep::MergeCheck)?;
        Ok(ancestor == of || self.ancestry.contains(&(ancestor.clone(), of.clone())))
    }
}

impl SnapshotLedger for World {
    type Error = FakeError;

    fn push_records(
        &self,
        _number: NonZeroU64,
        max: usize,
    ) -> Result<LedgerRecords<PushRecord>, Self::Error> {
        self.ledger_list(SnapshotStep::PushRecords, &self.pushes, max)
    }

    fn command_records(
        &self,
        _number: NonZeroU64,
        max: usize,
    ) -> Result<LedgerRecords<SnapshotCommand>, Self::Error> {
        self.ledger_list(SnapshotStep::CommandRecords, &self.commands, max)
    }
}

impl World {
    /// [`FakeError::Injected`] when the test made `step` fail.
    fn failing(&self, step: SnapshotStep) -> Result<(), FakeError> {
        match self.fail {
            Some(failing) if failing == step => Err(FakeError::Injected(step)),
            Some(_) | None => Ok(()),
        }
    }

    /// `records` as an adapter that reads one past `max` lists them, noting the cap asked for.
    fn ledger_list<T: Clone>(
        &self,
        step: SnapshotStep,
        records: &[T],
        max: usize,
    ) -> Result<LedgerRecords<T>, FakeError> {
        self.asked.borrow_mut().push((step, max));
        self.failing(step)?;
        if records.len() > max && !self.overfull_ledger {
            return Ok(LedgerRecords::TooMany);
        }
        Ok(LedgerRecords::Listed(records.to_vec()))
    }
}

impl CommandComments for World {
    type Error = FakeError;

    fn comment(&self, id: CommentId) -> Result<Option<IssueComment>, Self::Error> {
        Ok(self.comments.get(&id).cloned())
    }
}
