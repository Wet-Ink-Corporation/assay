//! What the pinned CLI says about itself (#83): the fields of `claude --version` and
//! `claude auth status` the preflight reads, bounded so they can go into an event as they are.

use thiserror::Error;

use crate::session_init::is_readable;

/// The fields of `claude --version` and `claude auth status` the preflight reads, as the runner
/// found them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusParts {
    /// The version `claude --version` printed.
    pub version: String,
    /// `loggedIn`.
    pub logged_in: bool,
    /// `authMethod`.
    pub auth_method: String,
    /// `apiProvider`.
    pub api_provider: String,
}

/// What the pinned CLI says about itself, within bounds.
///
/// Every name is 1 to [`MAX_BUILTIN_NAME_BYTES`](crate::MAX_BUILTIN_NAME_BYTES) bytes with no
/// control character, so it can go into an event as it is. Build it with [`CliStatus::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliStatus {
    parts: StatusParts,
}

/// Why no [`CliStatus`] could be read. Every variant fails the preflight.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StatusReadError {
    /// `claude --version` did not print one line `<version> (Claude Code)`.
    #[error("claude --version did not print `<version> (Claude Code)`")]
    Version,
    /// `claude auth status` is not one JSON object.
    #[error("claude auth status did not print one JSON object")]
    NotJson,
    /// An object in `claude auth status` repeats a key, so which value counts depends on the
    /// parser (RFC 8259 §4).
    #[error("claude auth status repeats a key")]
    RepeatedKey,
    /// A field the preflight reads is absent or not of the expected type.
    #[error("claude auth status field `{field}` is absent or malformed")]
    Field {
        /// The field.
        field: &'static str,
    },
    /// A name is empty, too long or holds a control character.
    #[error("`{field}` is not a readable name")]
    Name {
        /// The field (`version` for the CLI version).
        field: &'static str,
    },
}

impl CliStatus {
    /// Bounds `parts`.
    ///
    /// # Errors
    /// [`StatusReadError::Name`] naming the first of `version`, `authMethod` and `apiProvider`
    /// that is empty, over [`MAX_BUILTIN_NAME_BYTES`](crate::MAX_BUILTIN_NAME_BYTES) bytes or
    /// holds a control character.
    pub fn new(parts: StatusParts) -> Result<Self, StatusReadError> {
        let names = [
            ("version", &parts.version),
            ("authMethod", &parts.auth_method),
            ("apiProvider", &parts.api_provider),
        ];
        let unreadable = names
            .into_iter()
            .find_map(|(field, name)| (!is_readable(name)).then_some(field));
        match unreadable {
            Some(field) => Err(StatusReadError::Name { field }),
            None => Ok(Self { parts }),
        }
    }

    /// The CLI's version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.parts.version
    }

    /// Whether the CLI reports a logged-in session.
    #[must_use]
    pub const fn logged_in(&self) -> bool {
        self.parts.logged_in
    }

    /// How the session authenticates.
    #[must_use]
    pub fn auth_method(&self) -> &str {
        &self.parts.auth_method
    }

    /// Which API the session calls.
    #[must_use]
    pub fn api_provider(&self) -> &str {
        &self.parts.api_provider
    }

    /// The fields, for the event of a passed preflight.
    pub(super) fn into_parts(self) -> StatusParts {
        self.parts
    }
}
