//! The preflight of the LLM stage (`docs/architecture/06-operations.md` §1, 03 §8, R-03).
//!
//! It runs in two phases. [`preflight`], before any model call, refuses a job whose environment
//! holds a variable that selects another credential or provider, then asks the pinned CLI, under
//! the review calls' own environment, for its version and `claude auth status`: the version must
//! be the pin and the session logged in by OAuth token against Anthropic's own API. Its
//! [`AuthProof`] then judges the `system/init` of the first call with the clean-init rule of
//! #121 ([`check_init`]): built-ins are measured against the pinned allowlist, not required
//! absent.
//!
//! Every failure is the LLM-unavailable policy ([`Deferral::InfraLlmUnavailable`]) and carries
//! its `PreflightFailed` event; a pass carries `PreflightPassed` (03 §6). A field the preflight
//! did not get to observe is written [`UNOBSERVED`].

mod status;

use std::ffi::OsStr;
use std::fmt;

use assay_domain::{Deferral, EventPayload, Preflight};
use thiserror::Error;

use crate::config::{BuiltinAllowlist, ExactVersion};
use crate::session_init::{InitReadError, InitUnclean, SessionInit, check_init};

pub use status::{CliStatus, StatusParts, StatusReadError};

/// The `authMethod` of a session authenticated by `CLAUDE_CODE_OAUTH_TOKEN`.
pub const OAUTH_TOKEN_METHOD: &str = "oauth_token";

/// The `apiProvider` of Anthropic's own API.
pub const FIRST_PARTY_PROVIDER: &str = "firstParty";

/// The runbook for rotating the OAuth token (06 F4).
///
/// Auth failures and doctor's expiry warning link it. It is an absolute URL because the
/// failure is read in consumer repositories, which do not hold the document.
pub const TOKEN_ROTATION_RUNBOOK: &str =
    "https://github.com/Wet-Ink-Corporation/assay/blob/main/docs/operations/token-rotation.md";

/// What a preflight event says for a field the preflight did not get to observe.
pub const UNOBSERVED: &str = "unknown";

/// Prefixes of the variables that select a credential or provider other than the OAuth token:
/// API keys, auth tokens and base URLs (`ANTHROPIC_`), the Bedrock, Vertex and Foundry switches
/// and their cloud credentials (03 §8: unset and asserted).
const PROVIDER_PREFIXES: [&str; 6] = [
    "ANTHROPIC_",
    "CLAUDE_CODE_USE_",
    "AWS_",
    "GOOGLE_",
    "CLOUD_ML_",
    "VERTEX_",
];

/// Whether the variable `name` selects a credential or provider other than the OAuth token
/// (03 §8). The check is on bytes and case-sensitive, as the CLI reads names.
#[must_use]
pub fn is_provider_variable(name: &OsStr) -> bool {
    let name = name.as_encoded_bytes();
    PROVIDER_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix.as_bytes()))
}

/// Asks the pinned CLI for its version and `claude auth status` under the environment the model
/// calls run with. Implemented by `ClaudeHeadless` in `assay-runner`.
pub trait CliProbe {
    /// Why the CLI could not be asked, or its answer could not be read.
    type Error: std::error::Error;

    /// Runs `claude --version` and `claude auth status`.
    ///
    /// # Errors
    /// [`Self::Error`] when either command could not be run within its bounds or its output
    /// could not be read into a [`CliStatus`].
    fn status(&self) -> Result<CliStatus, Self::Error>;
}

/// Why the preflight failed. Each is the LLM-unavailable policy; names are shown escaped.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PreflightFailure<E> {
    /// A variable that selects another credential or provider is set in the job's environment
    /// (an API key outranks the OAuth token and silently bills the API). A name that is not
    /// UTF-8 is shown with replacement characters.
    #[error("{name:?} is set; credential and provider variables must be unset")]
    ProviderVariable {
        /// The variable.
        name: String,
    },
    /// The CLI could not be asked for its status, or its answer could not be read.
    #[error("the CLI's version and auth status could not be read")]
    Probe(#[source] E),
    /// The CLI is not the pinned version.
    #[error("the CLI is {found:?}, not the pinned {pinned}")]
    CliVersion {
        /// The version the CLI reported.
        found: String,
        /// The pinned version.
        pinned: String,
    },
    /// The CLI reports no logged-in session. Links [`TOKEN_ROTATION_RUNBOOK`].
    #[error("the CLI is not logged in; rotate the OAuth token: {TOKEN_ROTATION_RUNBOOK}")]
    NotLoggedIn,
    /// The session is not authenticated by OAuth token. Links [`TOKEN_ROTATION_RUNBOOK`].
    #[error(
        "the CLI authenticates with {found:?}, not {OAUTH_TOKEN_METHOD}; \
         rotate the OAuth token: {TOKEN_ROTATION_RUNBOOK}"
    )]
    AuthMethod {
        /// The method the CLI reported.
        found: String,
    },
    /// The session calls another API than Anthropic's own.
    #[error("the CLI calls provider {found:?}, not {FIRST_PARTY_PROVIDER}")]
    ApiProvider {
        /// The provider the CLI reported.
        found: String,
    },
    /// The first call's `system/init` could not be read (`init-unclean`).
    #[error("init-unclean: {0}")]
    InitUnreadable(InitReadError),
    /// The first call's `system/init` is not clean (`init-unclean`).
    #[error("init-unclean: {0}")]
    InitUnclean(InitUnclean),
}

impl<E> PreflightFailure<E> {
    /// The policy this failure takes: every one makes the LLM unavailable (06 §1, rubric
    /// rule 8a), deferred and requeued rather than reviewed without the preflight.
    #[must_use]
    pub const fn deferral(&self) -> Deferral {
        match self {
            Self::ProviderVariable { .. }
            | Self::Probe(_)
            | Self::CliVersion { .. }
            | Self::NotLoggedIn
            | Self::AuthMethod { .. }
            | Self::ApiProvider { .. }
            | Self::InitUnreadable(_)
            | Self::InitUnclean(_) => Deferral::InfraLlmUnavailable,
        }
    }

    /// The runbook that fixes this failure: [`TOKEN_ROTATION_RUNBOOK`] for an auth failure the
    /// token causes (no logged-in session, another auth method; 06 F4), `None` for the rest,
    /// which rotating the token does not fix.
    #[must_use]
    pub const fn runbook(&self) -> Option<&'static str> {
        match self {
            Self::NotLoggedIn | Self::AuthMethod { .. } => Some(TOKEN_ROTATION_RUNBOOK),
            Self::ProviderVariable { .. }
            | Self::Probe(_)
            | Self::CliVersion { .. }
            | Self::ApiProvider { .. }
            | Self::InitUnreadable(_)
            | Self::InitUnclean(_) => None,
        }
    }
}

/// A failed preflight: its `PreflightFailed` event and why. Its `source` is the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightFailed<E>(Box<Failed<E>>);

impl<E> fmt::Display for PreflightFailed<E>
where
    PreflightFailure<E>: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "preflight failed: {}", self.0.reason)
    }
}

impl<E: std::error::Error + 'static> std::error::Error for PreflightFailed<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0.reason)
    }
}

/// The contents of a [`PreflightFailed`], boxed so the `Err` of a preflight stays small.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failed<E> {
    event: Preflight,
    reason: PreflightFailure<E>,
}

impl<E> PreflightFailed<E> {
    fn new(event: Preflight, reason: PreflightFailure<E>) -> Self {
        Self(Box::new(Failed { event, reason }))
    }

    /// The data of the `PreflightFailed` event; `init_clean` is always `false`.
    #[must_use]
    pub fn event(&self) -> &Preflight {
        &self.0.event
    }

    /// Why the preflight failed.
    #[must_use]
    pub fn reason(&self) -> &PreflightFailure<E> {
        &self.0.reason
    }

    /// The policy the failure takes: [`Deferral::InfraLlmUnavailable`].
    #[must_use]
    pub fn deferral(&self) -> Deferral {
        self.0.reason.deferral()
    }

    /// The `PreflightFailed` event and the reason.
    #[must_use]
    pub fn into_parts(self) -> (EventPayload, PreflightFailure<E>) {
        let Failed { event, reason } = *self.0;
        (EventPayload::PreflightFailed(event), reason)
    }
}

/// The first phase of the preflight passed.
///
/// No credential or provider variable is set, and the pinned CLI is logged in by OAuth token
/// against Anthropic's API. Only [`preflight`] makes one; the first call's init is judged by
/// [`AuthProof::check_first_call`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthProof {
    status: CliStatus,
    pinned: ExactVersion,
}

/// The preflight passed; holds its `PreflightPassed` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightPassed {
    event: Preflight,
}

impl PreflightPassed {
    /// The data of the `PreflightPassed` event.
    #[must_use]
    pub const fn event(&self) -> &Preflight {
        &self.event
    }

    /// The `PreflightPassed` event.
    #[must_use]
    pub fn into_event(self) -> EventPayload {
        EventPayload::PreflightPassed(self.event)
    }
}

/// The first phase of the preflight, before any model call (06 §1).
///
/// `variables` are the names of the job's environment variables; any that
/// [`is_provider_variable`] fails the preflight, whatever its value (empty included). Then
/// `probe` is asked for the CLI's status, which must report the `pinned` version, a logged-in
/// session, `authMethod` [`OAUTH_TOKEN_METHOD`] and `apiProvider` [`FIRST_PARTY_PROVIDER`].
///
/// # Errors
/// [`PreflightFailed`] with the first rule broken, in the order above. Its event reports what
/// was observed, [`UNOBSERVED`] for the rest.
pub fn preflight<P: CliProbe>(
    variables: impl IntoIterator<Item = impl AsRef<OsStr>>,
    probe: &P,
    pinned: &ExactVersion,
) -> Result<AuthProof, PreflightFailed<P::Error>> {
    let unobserved = Preflight {
        auth_method: UNOBSERVED.to_owned(),
        cli_version: UNOBSERVED.to_owned(),
        init_clean: false,
        builtins: None,
    };
    let provider_variable = variables
        .into_iter()
        .find(|name| is_provider_variable(name.as_ref()));
    if let Some(name) = provider_variable {
        let name = name.as_ref().to_string_lossy().into_owned();
        return Err(PreflightFailed::new(
            unobserved,
            PreflightFailure::ProviderVariable { name },
        ));
    }
    let status = match probe.status() {
        Ok(status) => status,
        Err(error) => {
            return Err(PreflightFailed::new(
                unobserved,
                PreflightFailure::Probe(error),
            ));
        }
    };
    match judge_status(&status, pinned) {
        Some(reason) => Err(PreflightFailed::new(failed_event(&status, None), reason)),
        None => Ok(AuthProof {
            status,
            pinned: pinned.clone(),
        }),
    }
}

/// The first rule `status` breaks, if any.
fn judge_status<E>(status: &CliStatus, pinned: &ExactVersion) -> Option<PreflightFailure<E>> {
    if status.version() != pinned.as_str() {
        Some(PreflightFailure::CliVersion {
            found: status.version().to_owned(),
            pinned: pinned.as_str().to_owned(),
        })
    } else if !status.logged_in() {
        Some(PreflightFailure::NotLoggedIn)
    } else if status.auth_method() != OAUTH_TOKEN_METHOD {
        Some(PreflightFailure::AuthMethod {
            found: status.auth_method().to_owned(),
        })
    } else if status.api_provider() != FIRST_PARTY_PROVIDER {
        Some(PreflightFailure::ApiProvider {
            found: status.api_provider().to_owned(),
        })
    } else {
        None
    }
}

/// A failed event from what `status` and, when it was read, the first init showed.
fn failed_event(status: &CliStatus, init: Option<&SessionInit>) -> Preflight {
    Preflight {
        auth_method: status.auth_method().to_owned(),
        cli_version: init
            .map_or_else(|| status.version(), SessionInit::cli_version)
            .to_owned(),
        init_clean: false,
        builtins: init.map(SessionInit::builtin_set),
    }
}

impl AuthProof {
    /// How the CLI authenticates: [`OAUTH_TOKEN_METHOD`].
    #[must_use]
    pub fn auth_method(&self) -> &str {
        self.status.auth_method()
    }

    /// The CLI's version: the pin.
    #[must_use]
    pub fn cli_version(&self) -> &str {
        self.status.version()
    }

    /// The second phase: judges the `system/init` of the first model call (`init`, as the
    /// runner read it from that call's transcript) against the pinned version and `allowlist`
    /// with [`check_init`]. The call's output is not to be used unless this passes.
    ///
    /// # Errors
    /// [`PreflightFailed`] with [`PreflightFailure::InitUnreadable`] or
    /// [`PreflightFailure::InitUnclean`] (`init-unclean`). Its event carries the observed
    /// built-ins and the session's version when the init could be read.
    pub fn check_first_call<E>(
        self,
        init: Result<SessionInit, InitReadError>,
        allowlist: &BuiltinAllowlist,
    ) -> Result<PreflightPassed, PreflightFailed<E>> {
        let init = match init {
            Ok(init) => init,
            Err(error) => {
                return Err(PreflightFailed::new(
                    failed_event(&self.status, None),
                    PreflightFailure::InitUnreadable(error),
                ));
            }
        };
        if let Err(unclean) = check_init(&init, &self.pinned, allowlist) {
            return Err(PreflightFailed::new(
                failed_event(&self.status, Some(&init)),
                PreflightFailure::InitUnclean(unclean),
            ));
        }
        let status = self.status.into_parts();
        Ok(PreflightPassed {
            event: Preflight {
                auth_method: status.auth_method,
                cli_version: status.version,
                init_clean: true,
                builtins: Some(init.builtin_set()),
            },
        })
    }
}
