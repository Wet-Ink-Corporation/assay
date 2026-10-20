//! The GitHub adapter: REST and GraphQL over a [`Transport`] (`gh api` in production), with
//! retries (06 F8), bounded paging, and App identity decided by numeric id only (T7, ADR-11).
//!
//! [`GitHub`] implements `assay-app`'s [`Forge`](assay_app::Forge),
//! [`VerdictForge`](assay_app::VerdictForge), [`CommentForge`](assay_app::CommentForge),
//! [`StatusForge`](assay_app::StatusForge), [`PullRequestState`](assay_app::PullRequestState)
//! and [`ReconcileForge`](assay_app::ReconcileForge) for one repository, and over the `gh`
//! transport the [`BundleSource`](assay_app::BundleSource) run bundles are read from.
//! [`Api::mint_installation_token`] mints the narrowed token a job's [`GitHub`] is built with.
//! Every response is decoded into a wire DTO and converted, and data the adapter cannot read
//! exactly is refused ([`ForgeError`], wi-b5affa).

mod api;
mod artifacts;
mod base64;
mod checks;
mod comments;
mod dto;
mod error;
mod gh;
mod identity;
mod ledger;
mod ledger_refs;
mod ports;
mod pulls;
mod reads;
mod reconcile;
mod repository;
mod settings;
mod status;
mod token;
mod transport;

use assay_app::{AppId, UserId};

pub use api::{Api, MAX_LIST_ITEMS};
pub use artifacts::BundleFetchError;
pub use error::{ForgeError, Operation};
pub use gh::{
    GH_DOWNLOAD_TIMEOUT, GH_OUTPUT_CAP, GH_TIMEOUT, GhApi, GhCall, GhDownloadError, GhEnvironment,
    GhError, GhRunner, GhSetupError, GhToken, InvalidToken,
};
pub use identity::MAX_COMMITS;
pub use ledger::{LedgerRepo, LedgerRepoError};
pub use ledger_refs::{LedgerRefsError, LedgerRefsRepo, LedgerRemoteError, ledger_push_remote};
pub use repository::{ParseRepositoryError, Repository};
pub use token::{Access, InstallationId, InstallationToken, Permission, ScopeError, TokenScope};
pub use transport::{
    ApiPath, InvalidPath, MAX_RESPONSE_BYTES, Method, Request, Response, Sleep, ThreadSleep,
    Transport,
};

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

    /// The API back, to close its transport.
    #[must_use]
    pub fn into_api(self) -> Api<T, S> {
        self.api
    }

    /// `repos/<owner>/<name>/<rest>`, for `operation`.
    fn path<E>(&self, operation: Operation, rest: &str) -> Result<ApiPath, ForgeError<E>> {
        api_path(operation, format!("repos/{}/{rest}", self.repository))
    }
}

/// `path` as an [`ApiPath`], refused for `operation` before anything is sent.
fn api_path<E>(operation: Operation, path: String) -> Result<ApiPath, ForgeError<E>> {
    ApiPath::new(path).map_err(|InvalidPath| ForgeError::InvalidRequest {
        operation,
        detail: "the path is not a plain API path",
    })
}
