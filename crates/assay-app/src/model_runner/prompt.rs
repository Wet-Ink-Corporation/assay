//! The user prompt of a model call.

use std::fmt;

/// A user prompt the headless CLI reads as its positional argument (03 §8): non-empty, not
/// starting with `-` (which the CLI would parse as a flag) and free of NUL bytes (which no
/// argument can carry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt(String);

/// Why a text cannot be a [`Prompt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PromptError {
    /// The text is empty.
    #[error("the prompt is empty")]
    Empty,
    /// The text starts with `-`, so the CLI would read it as an option.
    #[error("the prompt starts with '-'")]
    LeadingDash,
    /// The text holds a NUL byte, which a process argument cannot carry.
    #[error("the prompt holds a NUL byte")]
    Nul,
}

/// A rejected prompt: why, and the text handed back so the caller need not keep a copy (ERR-14).
/// `Debug` and `Display` show only the reason.
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct InvalidPrompt {
    reason: PromptError,
    text: String,
}

impl InvalidPrompt {
    /// Why the text was rejected.
    #[must_use]
    pub const fn reason(&self) -> PromptError {
        self.reason
    }

    /// The rejected text.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.text
    }
}

impl fmt::Debug for InvalidPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InvalidPrompt")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl Prompt {
    /// Checks `text` and takes it as a prompt.
    ///
    /// # Errors
    /// Returns [`InvalidPrompt`] holding the text and the reason: [`PromptError::Empty`],
    /// [`PromptError::LeadingDash`] or [`PromptError::Nul`].
    pub fn new(text: impl Into<String>) -> Result<Self, InvalidPrompt> {
        let text = text.into();
        let reason = if text.is_empty() {
            PromptError::Empty
        } else if text.starts_with('-') {
            PromptError::LeadingDash
        } else if text.contains('\0') {
            PromptError::Nul
        } else {
            return Ok(Self(text));
        };
        Err(InvalidPrompt { reason, text })
    }

    /// The prompt text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
