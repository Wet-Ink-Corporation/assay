//! The GitHub adapter: REST and GraphQL over a [`Transport`] (`gh api` in production), with
//! retries (06 F8), bounded paging, and App identity decided by numeric id only (T7, ADR-11).
//!
//! [`GitHub`] implements `assay-app`'s [`Forge`](assay_app::Forge),
//! [`VerdictForge`](assay_app::VerdictForge), [`CommentForge`](assay_app::CommentForge),
//! [`StatusForge`](assay_app::StatusForge) and [`PullRequestState`](assay_app::PullRequestState)
//! for one repository. [`Api::mint_installation_token`] mints the narrowed token a job's
//! [`GitHub`] is built with. Every response is decoded into a wire DTO and converted, and data
//! the adapter cannot read exactly is refused ([`ForgeError`], wi-b5affa).

mod api;
mod checks;
mod comments;
mod dto;
mod error;
mod identity;
mod ports;
mod pulls;
mod reads;
mod repository;
mod status;
mod token;
mod transport;

use assay_app::{AppId, UserId};

pub use api::{Api, MAX_LIST_ITEMS};
pub use error::{ForgeError, Operation};
pub use identity::MAX_COMMITS;
pub use repository::{ParseRepositoryError, Repository};
pub use token::{Access, InstallationId, InstallationToken, Permission, ScopeError, TokenScope};
pub use transport::{Method, Request, Response, Sleep, ThreadSleep, Transport};

/// GitHub as one App sees one repository.
#[derive(Debug)]
pub struct GitHub<T, S> {
    api: Api<T, S>,
    repository: Repository,
    identity: AppIdentity,
}

/// Who "the App" is: its id, which authors its check runs, and the id of its `[bot]` user,
/// which authors its comments. Both are numbers GitHub assigns; no name is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppIdentity {
    /// The App's id (`check_run.app.id`, the ruleset's `integration_id`).
    pub app: AppId,
    /// The id of the App's `<slug>[bot]` user.
    pub bot: UserId,
}

impl<T, S> GitHub<T, S> {
    /// The adapter for `repository`, calling `api` with a token of the App `identity` names.
    pub const fn new(api: Api<T, S>, repository: Repository, identity: AppIdentity) -> Self {
        Self {
            api,
            repository,
            identity,
        }
    }

    /// `repos/<owner>/<name>/<rest>`.
    fn path(&self, rest: &str) -> String {
        format!("repos/{}/{rest}", self.repository)
    }
}
