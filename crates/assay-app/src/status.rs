//! `assay status` (#126, `docs/architecture/03-contracts.md` §6b): a PR's assay state as one
//! versioned contract, `assay.status/1`, so the mod and scripts never parse check markdown, PR
//! comments or `gh` output (ADR-23's mod rules, #125).
//!
//! It reads GitHub through one narrow port, [`StatusForge`], and the run's evidence bundle
//! through [`BundleSource`]. Only the configured App's checks count: a same-named check from
//! any other app is ignored. The decision is read only from the bundle the verdict check's
//! `output.text` pointer names, once the bundle hashes to the pointer and carries the check's
//! decision key. Text in the output is only what assay already published, sanitised (04 §6,
//! T8): titles and bodies from the App's own inline comments, as escaped for GitHub, and
//! reason details, rules and paths cleaned as the summary cleans them. Anything read that does
//! not have exactly the shape assay or GitHub writes is refused (wi-b5affa).

mod document;
mod fetched_at;
mod findings;
mod ports;
mod verdict;

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use assay_domain::Rubric;
use thiserror::Error;

pub use self::fetched_at::{FetchedAt, InvalidFetchedAt};
pub use self::ports::{
    AppId, CheckView, ObservedConclusion, PullView, ReviewCommentView, StatusForge, UserId, Viewer,
};
use crate::config::{CommitId, GateName};
use crate::contract::{Contract, ContractValidator, ReadContractError};
use crate::publish::{MAX_PRIOR_COMMENTS, is_hidden};
use crate::replay::{BundleShape, BundleSource};

/// The most check runs on a head `status` reads; more are refused (H-13). A PR's workflows can
/// add check runs, so the count is not assay's to bound.
pub const MAX_CHECK_RUNS: usize = 1000;

/// The most findings one status lists, gate and reviewer findings together; more are refused
/// (H-13).
pub const MAX_FINDINGS: usize = 4096;

/// The most reasons, and the most degraded roles, one status lists; more are refused (H-13).
pub const MAX_REASONS: usize = 1024;

/// How many open PRs of a branch are read: one more than the one accepted.
const OPEN_PULLS_READ: usize = 2;

/// The longest head branch name accepted, in bytes (the schema's cap).
const MAX_BRANCH_BYTES: usize = 255;

/// The repository a status is for, as the caller's configuration names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRepository {
    /// Its numeric id, which keys the run's artifacts.
    pub id: NonZeroU64,
    /// `owner/name`.
    pub name: String,
    /// Its default branch, which names the verdict check (R3-03).
    pub default_branch: String,
}

/// Which PR to report on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullSelector<'a> {
    /// `--pr <n>`.
    Number(NonZeroU64),
    /// The one open PR whose head is this branch (the current branch, without `--pr`).
    Branch(&'a str),
}

/// How much of each finding to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Detail {
    /// Without bodies.
    Summary,
    /// `--full`: each published finding's body too.
    Full,
}

/// The P10 identity lists of base config, by numeric GitHub user id (03 §1). `assay status`
/// only reports them about the viewer (#137); decide-only re-checks every action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identities {
    /// `.assay/owners` `owners` and `approvers`: the privileged identities.
    pub privileged: BTreeSet<UserId>,
    /// `.assay/agents`.
    pub agents: BTreeSet<UserId>,
}

/// What `status` is asked.
#[derive(Debug, Clone, Copy)]
pub struct StatusRequest<'a> {
    /// The repository.
    pub repository: &'a StatusRepository,
    /// The `assay-verdict` App's id: only its checks and comments count.
    pub app: AppId,
    /// The PR.
    pub pull: PullSelector<'a>,
    /// The local checkout's HEAD, if any; a different head makes the status `stale`.
    pub local_head: Option<&'a CommitId>,
    /// How much of each finding to show.
    pub detail: Detail,
    /// The rubric the bundle was decided under: it says which findings are waivable.
    pub rubric: &'a Rubric,
    /// The base P10 identity lists.
    pub identities: &'a Identities,
    /// When the state was read.
    pub fetched_at: &'a FetchedAt,
}

/// The ports `status` reads through, borrowed for one call.
#[derive(Debug)]
pub struct StatusPorts<'p, F, B> {
    /// GitHub, as the viewer.
    pub forge: &'p F,
    /// The run's artifacts.
    pub bundles: &'p B,
}

/// A PR's status: the `assay.status/1` document, validated against its schema, and the same
/// facts as plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    json: Vec<u8>,
    text: String,
}

impl StatusReport {
    /// The `assay.status/1` document: pretty JSON and a final newline.
    #[must_use]
    pub fn json(&self) -> &[u8] {
        &self.json
    }

    /// The same facts for a person: one fact per line, ending in a newline.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The read that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusStep {
    /// Reading the PR named by number.
    Pull,
    /// Listing the open PRs of a branch.
    OpenPulls,
    /// Listing the head's check runs.
    CheckRuns,
    /// Listing the App's review comments.
    ReviewComments,
    /// Reading the viewer.
    Viewer,
}

impl StatusStep {
    const fn describe(self) -> &'static str {
        match self {
            Self::Pull => "read the PR",
            Self::OpenPulls => "list the branch's open PRs",
            Self::CheckRuns => "list the head's check runs",
            Self::ReviewComments => "list the App's review comments",
            Self::Viewer => "read the viewer",
        }
    }
}

/// What GitHub returned that does not have the shape assay or GitHub writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusUnreadable {
    /// A head branch that is empty, over 255 bytes or holds a control or format character.
    Branch,
    /// A page URL that is not a `https://github.com/<repository>/` page.
    Url,
    /// A viewer login that is not a GitHub login.
    Login,
    /// An App check named `assay / gate: <name>` whose name is not a plugin name.
    GateName,
    /// An App check whose status and conclusion disagree (completed without one, or with one
    /// before completing).
    CheckState,
    /// A verdict check whose `external_id` is neither a decision key nor `open`'s stamp.
    ExternalId,
    /// A decided verdict check whose `output.text` is not what `publish` writes, or names
    /// another decision key.
    CheckText,
}

impl StatusUnreadable {
    const fn describe(self) -> &'static str {
        match self {
            Self::Branch => "head branch",
            Self::Url => "page URL",
            Self::Login => "viewer login",
            Self::GateName => "gate check name",
            Self::CheckState => "check status and conclusion",
            Self::ExternalId => "verdict check external id",
            Self::CheckText => "verdict check output text",
        }
    }
}

/// Why the decision's evidence bundle could not be read. Never a decision.
#[derive(Debug, Error)]
pub enum BundleProblem<B> {
    /// The pointer names a re-run: bundles are persisted by attempt 1 only (R3-05).
    #[error("the evidence pointer names a re-run")]
    NotFirstAttempt,
    /// The artifact store could not be read.
    #[error("could not read the artifact store")]
    Fetch(#[source] B),
    /// The bundle is gone (F20).
    #[error("the bundle is missing")]
    Missing,
    /// The bundle is past its retention.
    #[error("the bundle has expired")]
    Expired,
    /// The bundle is over the cap.
    #[error("the bundle is over its size cap")]
    TooLarge,
    /// The bundle does not hash to the pointer (F19).
    #[error("the bundle does not hash to the evidence pointer")]
    Digest,
    /// The bundle is not `assay.run-inputs/1`.
    #[error("the bundle is not assay.run-inputs/1: {0}")]
    Shape(BundleShape),
    /// The bundle disagrees with the pointer or with itself.
    #[error("the bundle disagrees with its pointer or with itself")]
    Inconsistent,
    /// The bundle's decision is not the one the check carries.
    #[error("the bundle's decision is not the verdict check's")]
    Decision,
    /// The bundle was decided under another rubric version than the one given.
    #[error("the bundle was decided under another rubric version")]
    RubricVersion,
}

/// Why `status` could not report. Nothing is printed.
#[derive(Debug, Error)]
pub enum StatusError<F, B> {
    /// The repository's name is not `owner/name`.
    #[error("the repository name is not `owner/name`")]
    Repository,
    /// A GitHub read failed.
    #[error("could not {}", step.describe())]
    Forge {
        /// The read.
        step: StatusStep,
        /// The forge's error.
        #[source]
        source: F,
    },
    /// The PR named by number does not exist.
    #[error("PR #{number} does not exist")]
    NoSuchPull {
        /// The number asked for.
        number: NonZeroU64,
    },
    /// No open PR has the branch as its head.
    #[error("no open PR has the branch as its head; name one with --pr")]
    NoOpenPull,
    /// Several open PRs have the branch as their head (the list is read only to the second).
    #[error("more than one open PR has the branch as its head; name one with --pr")]
    AmbiguousBranch,
    /// GitHub returned something that does not read.
    #[error("GitHub returned a {} that does not read", .0.describe())]
    Unreadable(StatusUnreadable),
    /// More check runs on the head than [`MAX_CHECK_RUNS`].
    #[error("{count} check runs on the head, over the {MAX_CHECK_RUNS} cap")]
    TooManyChecks {
        /// How many.
        count: usize,
    },
    /// More App review comments than [`MAX_PRIOR_COMMENTS`].
    #[error("{count} App review comments, over the {MAX_PRIOR_COMMENTS} cap")]
    TooManyComments {
        /// How many.
        count: usize,
    },
    /// More findings than [`MAX_FINDINGS`].
    #[error("{count} findings, over the {MAX_FINDINGS} cap")]
    TooManyFindings {
        /// How many.
        count: usize,
    },
    /// More reasons or degraded roles than [`MAX_REASONS`].
    #[error("{count} reasons or degraded roles, over the {MAX_REASONS} cap")]
    TooManyReasons {
        /// How many.
        count: usize,
    },
    /// Several App checks carry the verdict check's name on the head (A-27).
    #[error("{count} App verdict checks on the head")]
    DuplicateVerdict {
        /// How many.
        count: usize,
    },
    /// Several App checks carry one gate's name on the head.
    #[error("several App checks for gate {plugin}")]
    DuplicateGate {
        /// The plugin.
        plugin: GateName,
    },
    /// The decision's evidence bundle could not be read.
    #[error("could not read the decision: {0}")]
    Bundle(BundleProblem<B>),
    /// The document could not be encoded: a defect in this crate.
    #[error("could not encode the status")]
    Encode(#[source] serde_json::Error),
    /// The document does not satisfy `status-1.schema.json`: a defect in this crate (every
    /// input it copies is parsed first, [`FetchedAt`] included).
    #[error("the status does not satisfy its schema")]
    Invalid(#[source] ReadContractError),
}

/// The [`StatusError`] of [`status`] over `ports`' error types.
pub type StatusFailure<F, B> = StatusError<<F as StatusForge>::Error, <B as BundleSource>::Error>;

/// Reports on `request`'s PR, reading GitHub and the artifact store through `ports`, and
/// validates the document against `assay.status/1` before returning it.
///
/// # Examples
///
/// ```
/// use std::io::{self, Write};
///
/// use assay_app::{
///     BundleSource, ContractValidator, StatusForge, StatusPorts, StatusRequest, status,
/// };
///
/// /// Writes the status document of `request`'s PR to `out`, or why there is none.
/// fn write_status<F: StatusForge, B: BundleSource>(
///     request: &StatusRequest<'_>,
///     ports: &StatusPorts<'_, F, B>,
///     validator: &ContractValidator,
///     mut out: impl Write,
/// ) -> io::Result<()> {
///     match status(request, ports, validator) {
///         Ok(report) => out.write_all(report.json()),
///         Err(error) => writeln!(out, "no status: {error}"),
///     }
/// }
/// ```
///
/// # Errors
///
/// [`StatusError`] when a read fails, the PR cannot be found or chosen, something read does not
/// have its expected shape, a count is over its cap, or the decision's bundle cannot be read.
pub fn status<F: StatusForge, B: BundleSource>(
    request: &StatusRequest<'_>,
    ports: &StatusPorts<'_, F, B>,
    validator: &ContractValidator,
) -> Result<StatusReport, StatusFailure<F, B>> {
    let repository = &request.repository.name;
    if crate::publish::Repository::parse(repository).is_none() {
        return Err(StatusError::Repository);
    }
    let pull = pull(request, ports.forge)?;
    let observed = verdict::observe(request, ports, &pull)?;
    let findings = match &observed.decided {
        Some(decided) => findings::findings(request, ports.forge, &pull, decided)?,
        None => Vec::new(),
    };
    let viewer = ports.forge.viewer().map_err(|source| StatusError::Forge {
        step: StatusStep::Viewer,
        source,
    })?;
    let facts = document::Facts {
        pull: &pull,
        observed,
        findings,
        viewer: &viewer,
    };
    let document = document::Document::new(request, facts)?;
    let mut json = serde_json::to_vec_pretty(&document).map_err(StatusError::Encode)?;
    json.push(b'\n');
    validator
        .read(Contract::Status, &json)
        .map_err(StatusError::Invalid)?;
    Ok(StatusReport {
        text: document.text(),
        json,
    })
}

/// The PR `request` names: by number, or the one open PR of the branch. Its head branch must
/// read as one.
fn pull<F: StatusForge, B>(
    request: &StatusRequest<'_>,
    forge: &F,
) -> Result<PullView, StatusError<F::Error, B>> {
    let pull = match request.pull {
        PullSelector::Number(number) => forge
            .pull(number)
            .map_err(|source| StatusError::Forge {
                step: StatusStep::Pull,
                source,
            })?
            .ok_or(StatusError::NoSuchPull { number })?,
        PullSelector::Branch(branch) => {
            let mut open = forge
                .open_pulls(branch, OPEN_PULLS_READ)
                .map_err(|source| StatusError::Forge {
                    step: StatusStep::OpenPulls,
                    source,
                })?;
            match open.len() {
                0 => return Err(StatusError::NoOpenPull),
                1 => open.pop().ok_or(StatusError::NoOpenPull)?,
                _ => return Err(StatusError::AmbiguousBranch),
            }
        }
    };
    let branch = &pull.head_ref;
    let readable =
        !branch.is_empty() && branch.len() <= MAX_BRANCH_BYTES && !branch.chars().any(is_hidden);
    if readable {
        Ok(pull)
    } else {
        Err(StatusError::Unreadable(StatusUnreadable::Branch))
    }
}

/// `url` when it is a page of `repository` on github.com, in the characters the schema allows.
fn page(url: &str, repository: &str) -> Result<String, StatusUnreadable> {
    /// The longest page URL accepted.
    const MAX_URL_BYTES: usize = 2048;
    let on_site = url
        .strip_prefix("https://github.com/")
        .and_then(|rest| rest.strip_prefix(repository))
        .is_some_and(|rest| rest.starts_with('/'));
    let allowed = |byte: u8| matches!(byte, b'!' | b'#'..=b';' | b'=' | b'?'..=b'[' | b']' | b'_' | b'a'..=b'z' | b'~');
    if on_site && url.len() <= MAX_URL_BYTES && url.bytes().all(allowed) {
        // The document owns its copy of the page.
        Ok(url.to_owned())
    } else {
        Err(StatusUnreadable::Url)
    }
}
