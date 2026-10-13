//! The environment of a headless call (03 §8): cleared, then the OAuth token, the subprocess
//! scrub flag and the variables the composition root passes on purpose.

use std::ffi::OsString;
use std::fmt;
use std::process::Command;

/// The variable carrying the OAuth token.
const OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The variable that scrubs credentials from the CLI's subprocesses (03 §8: moot with no
/// process-spawning tool, kept as belt and braces).
const SUBPROCESS_ENV_SCRUB: &str = "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB";

/// Prefixes of variables that select another credential or provider: API keys and auth tokens,
/// and the Bedrock, Vertex and Foundry switches with their cloud credentials (03 §8: unset).
const RESERVED_PREFIXES: [&str; 6] = [
    "ANTHROPIC_",
    "CLAUDE_CODE_USE_",
    "AWS_",
    "GOOGLE_",
    "CLOUD_ML_",
    "VERTEX_",
];

/// The environment a `claude -p` call starts with; nothing is inherited.
///
/// The call sees the OAuth token, `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` and the variables passed
/// with [`pass`](Self::pass) (such as `HOME` and `PATH`), and nothing else. API keys, auth tokens
/// and cloud-provider variables cannot be passed, so they are unset by construction.
///
/// `Debug` shows variable names only: the token and the values may be secrets.
#[derive(Clone)]
pub struct HeadlessEnv {
    oauth_token: String,
    passed: Vec<(String, OsString)>,
}

/// Why a variable cannot be passed to a headless call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvVarError {
    /// The name is not a plain identifier (`[A-Za-z_][A-Za-z0-9_]*`).
    #[error("{name:?} is not a valid environment variable name")]
    InvalidName {
        /// The rejected name.
        name: String,
    },
    /// The name is set by the runner itself, or selects another credential or provider.
    #[error("{name} is reserved by the headless profile")]
    Reserved {
        /// The rejected name.
        name: String,
    },
}

impl HeadlessEnv {
    /// An environment holding only `oauth_token` (and the scrub flag).
    #[must_use]
    pub fn new(oauth_token: impl Into<String>) -> Self {
        Self {
            oauth_token: oauth_token.into(),
            passed: Vec::new(),
        }
    }

    /// Passes `name=value` to every call. A later value for the same name replaces the earlier.
    ///
    /// # Errors
    /// Returns [`EnvVarError::InvalidName`] for a name that is not a plain identifier, and
    /// [`EnvVarError::Reserved`] for the token, the scrub flag, and any `ANTHROPIC_`,
    /// `CLAUDE_CODE_USE_`, `AWS_`, `GOOGLE_`, `CLOUD_ML_` or `VERTEX_` variable. The value is
    /// dropped, never echoed back.
    pub fn pass(&mut self, name: &str, value: impl Into<OsString>) -> Result<(), EnvVarError> {
        if !is_identifier(name) {
            return Err(EnvVarError::InvalidName {
                name: name.to_owned(),
            });
        }
        let reserved = name == OAUTH_TOKEN
            || name == SUBPROCESS_ENV_SCRUB
            || RESERVED_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix));
        if reserved {
            return Err(EnvVarError::Reserved {
                name: name.to_owned(),
            });
        }
        self.passed.push((name.to_owned(), value.into()));
        Ok(())
    }

    /// Replaces `command`'s environment with this one.
    pub(super) fn apply(&self, command: &mut Command) {
        command
            .env_clear()
            .envs(self.passed.iter().map(|(name, value)| (name, value)))
            .env(OAUTH_TOKEN, &self.oauth_token)
            .env(SUBPROCESS_ENV_SCRUB, "1");
    }
}

impl fmt::Debug for HeadlessEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadlessEnv")
            .field(
                "passed",
                &self.passed.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
fn is_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
