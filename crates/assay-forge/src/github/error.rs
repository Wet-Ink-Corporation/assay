//! Why a GitHub call failed: transport failures apart from GitHub's rejections (ERR-10), and
//! refusals of data the adapter will not interpret (wi-b5affa).

use std::fmt;

use assay_app::{AppId, CheckRunId};

/// Why a call to GitHub failed. Every variant fails closed: nothing is inferred from a
/// response the adapter could not read exactly.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError<E> {
    /// No response was received.
    #[error("{operation}: GitHub could not be reached")]
    Transport {
        /// The call.
        operation: Operation,
        /// The transport's failure.
        #[source]
        source: E,
    },
    /// GitHub refused the request with a status that is not retried.
    #[error("{operation}: GitHub refused the request (HTTP {status})")]
    Rejected {
        /// The call.
        operation: Operation,
        /// The HTTP status.
        status: u16,
    },
    /// GitHub kept failing with a retryable status (5xx or a secondary rate limit) after every
    /// backoff: the publication fails as `publish-error` (06 F8) and `reconcile` fixes it.
    #[error(
        "publish-error: {operation}: GitHub kept failing (HTTP {status}) after {attempts} attempts"
    )]
    Exhausted {
        /// The call.
        operation: Operation,
        /// The last HTTP status.
        status: u16,
        /// How many requests were sent.
        attempts: u8,
    },
    /// The call's [`Deadline`](assay_app::Deadline), the publish lock lease's expiry, came
    /// before its next request or retry wait could end, so neither was made (#290). Requests
    /// sent earlier may have landed. Like [`Exhausted`](Self::Exhausted), a `publish-error`.
    #[error("publish-error: {operation}: stopped at the publish lock's deadline")]
    PastDeadline {
        /// The call.
        operation: Operation,
    },
    /// The response is not what the API documents.
    #[error("{operation}: unexpected response from GitHub: {detail}")]
    Malformed {
        /// The call.
        operation: Operation,
        /// What was wrong.
        detail: &'static str,
    },
    /// The response is not JSON of the documented shape, or repeats a key.
    #[error("{operation}: unreadable response from GitHub")]
    Json {
        /// The call.
        operation: Operation,
        /// The parse failure.
        #[source]
        source: serde_json::Error,
    },
    /// A list is longer than the adapter reads; it is refused rather than cut.
    #[error("{operation}: more than {cap} items")]
    TooMany {
        /// The call.
        operation: Operation,
        /// The most items accepted.
        cap: usize,
    },
    /// A response body is longer than [`MAX_RESPONSE_BYTES`](super::MAX_RESPONSE_BYTES); it is
    /// refused unparsed (H-13).
    #[error("{operation}: the response is over {cap} bytes")]
    ResponseTooLarge {
        /// The call.
        operation: Operation,
        /// The most bytes accepted.
        cap: usize,
    },
    /// The request was refused before anything was sent: an argument it cannot be built from
    /// safely, such as an empty marker or a path that is not a plain API path.
    #[error("{operation}: refused before sending: {detail}")]
    InvalidRequest {
        /// The call.
        operation: Operation,
        /// What was wrong.
        detail: &'static str,
    },
    /// The App authored more than one check or comment where there must be at most one.
    #[error("{operation}: the App authored more than one match")]
    Ambiguous {
        /// The call.
        operation: Operation,
    },
    /// The check is not the App's, so it is never written.
    #[error("check run {} was not created by the App", id.0)]
    ForeignCheck {
        /// The check.
        id: CheckRunId,
    },
    /// The adapter was asked about an app other than the one it is configured for.
    #[error("app {} is not the configured App", app.0)]
    UnknownApp {
        /// The app asked for.
        app: AppId,
    },
    /// A minted token's permissions or repositories differ from the request.
    #[error("the minted token's scope differs from the request")]
    ScopeMismatch,
}

/// The GitHub call a [`ForgeError`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// Listing the check runs on a commit.
    ListChecks,
    /// Reading one check run.
    ReadCheck,
    /// Creating a check run.
    CreateCheck,
    /// Updating a check run.
    UpdateCheck,
    /// Listing a PR's issue comments.
    ListComments,
    /// Posting an issue comment.
    CreateComment,
    /// Editing an issue comment.
    UpdateComment,
    /// Listing a PR's review comments.
    ListReviewComments,
    /// Posting a review comment.
    CreateReviewComment,
    /// Listing a PR's reviews.
    ListReviews,
    /// Listing a PR's labels.
    ListLabels,
    /// Listing a comment's reactions.
    ListReactions,
    /// Reading a PR's timeline.
    ReadTimeline,
    /// Resolving commits' authors and committers.
    ResolveCommits,
    /// Minting an installation token.
    MintToken,
    /// Reading a PR.
    ReadPull,
    /// Listing PRs.
    ListPulls,
    /// Reading or listing workflow runs.
    ListRuns,
    /// Reading the authenticated user.
    ReadViewer,
    /// Reading the repository.
    ReadRepository,
    /// Reading a file or directory.
    ReadContents,
    /// Listing the rulesets.
    ListRulesets,
    /// Reading one ruleset.
    ReadRuleset,
    /// Reading the rules in force on a branch.
    ReadBranchRules,
    /// Reading an environment.
    ReadEnvironment,
    /// Listing an environment's deployment branch policies.
    ListBranchPolicies,
    /// Reading the Actions token's rights.
    ReadWorkflowPermissions,
    /// Listing collaborators.
    ListCollaborators,
    /// Reading a user by id.
    ReadUser,
    /// Reading a user's permission.
    ReadPermission,
    /// Reading a file of the ledger repository.
    ReadLedgerFile,
    /// Writing a file of the ledger repository.
    WriteLedgerFile,
    /// Reading a lease ref of the ledger repository and its commit.
    ReadLedgerRef,
    /// Creating a lease ref of the ledger repository at a new commit.
    CreateLedgerRef,
    /// Listing the publish lock refs of the ledger repository.
    ListLedgerRefs,
    /// Listing a workflow run's artifacts.
    ListArtifacts,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ListChecks => "list check runs",
            Self::ReadCheck => "read check run",
            Self::CreateCheck => "create check run",
            Self::UpdateCheck => "update check run",
            Self::ListComments => "list comments",
            Self::CreateComment => "create comment",
            Self::UpdateComment => "update comment",
            Self::ListReviewComments => "list review comments",
            Self::CreateReviewComment => "create review comment",
            Self::ListReviews => "list reviews",
            Self::ListLabels => "list labels",
            Self::ListReactions => "list reactions",
            Self::ReadTimeline => "read timeline",
            Self::ResolveCommits => "resolve commits",
            Self::MintToken => "mint installation token",
            Self::ReadPull => "read pull request",
            Self::ListPulls => "list pull requests",
            Self::ListRuns => "list workflow runs",
            Self::ReadViewer => "read viewer",
            Self::ReadRepository => "read repository",
            Self::ReadContents => "read contents",
            Self::ListRulesets => "list rulesets",
            Self::ReadRuleset => "read ruleset",
            Self::ReadBranchRules => "read branch rules",
            Self::ReadEnvironment => "read environment",
            Self::ListBranchPolicies => "list deployment branch policies",
            Self::ReadWorkflowPermissions => "read workflow permissions",
            Self::ListCollaborators => "list collaborators",
            Self::ReadUser => "read user",
            Self::ReadPermission => "read permission",
            Self::ReadLedgerFile => "read ledger file",
            Self::WriteLedgerFile => "write ledger file",
            Self::ReadLedgerRef => "read ledger ref",
            Self::CreateLedgerRef => "create ledger ref",
            Self::ListLedgerRefs => "list ledger refs",
            Self::ListArtifacts => "list run artifacts",
        })
    }
}
