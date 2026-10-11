//! The `[assay]` table of `.assay/plugins.toml` (`docs/architecture/03-contracts.md` §1).

use serde::Deserialize;

use super::pin::Sha256Digest;

/// Repository-wide settings from the `[assay]` table. Every field is individually valid; none
/// constrains another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssaySettings {
    /// The assay OCI artifact, by digest.
    pub digest: Sha256Digest,
    /// The warm `target/` OCI artifact for Z2b jobs, by digest (`06 §3`).
    pub warm_target: Option<Sha256Digest>,
    /// The exact Claude Code CLI version to install.
    pub claude_cli: Option<ExactVersion>,
    /// How the model is reached.
    pub runner: Option<RunnerKind>,
    /// Bases a verdict is published for; any other base is `failure (base-not-protected)`.
    /// Empty by default, which protects nothing (fail-safe).
    pub protected_bases: Vec<String>,
    /// Whether verified LLM blockers fail the check (`QA-05`); advisory by default.
    pub llm_blockers: Promotion,
    /// Whether verified LLM majors over the threshold fail the check; advisory by default.
    pub llm_majors: Promotion,
    /// What an unavailable LLM stage does; `warn` by default (`H-04`). Never applies to gates.
    pub on_infra_failure: InfraFailurePolicy,
    /// PR classes that always block when the LLM stage is unavailable; both by default (`H-04`).
    pub block_on_infra_for: Vec<PrClass>,
    /// The central cost ledger repository, `owner/name` (ADR-19).
    pub ledger_repo: Option<String>,
}

/// How the review stage reaches the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerKind {
    /// Claude Code with the owner's OAuth token.
    ClaudeOauth,
    /// Claude Code with an API key.
    ClaudeApi,
}

/// Whether an LLM finding class may fail the check (promotion rule `QA-05`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Promotion {
    /// Published and counted, never failing the check.
    #[default]
    Advisory,
    /// Fails the check.
    Block,
}

/// What happens to the LLM stage's verdict when the stage is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InfraFailurePolicy {
    /// Publish without the LLM stage.
    #[default]
    Warn,
    /// Defer, then fail.
    Block,
}

/// The closed vocabulary of PR classes (`docs/architecture/03-contracts.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrClass {
    /// A changed file matches the trust-path list.
    TrustPath,
    /// A change adds or alters code that runs at build time.
    ExecSurface,
}

/// An exact release version, `<major>.<minor>.<patch>` in decimal digits; never a range.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExactVersion(String);

impl ExactVersion {
    /// Accepts `text` when it is three dot-separated runs of decimal digits, handing it back
    /// otherwise.
    pub(crate) fn new(text: String) -> Result<Self, String> {
        let digits =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        let mut parts = text.split('.');
        let valid = matches!(
            (parts.next(), parts.next(), parts.next(), parts.next()),
            (Some(major), Some(minor), Some(patch), None)
                if digits(major) && digits(minor) && digits(patch)
        );
        if valid { Ok(Self(text)) } else { Err(text) }
    }

    /// The version as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `block_on_infra_for` when the file omits it (`H-04`).
pub(super) fn default_block_on_infra_for() -> Vec<PrClass> {
    vec![PrClass::TrustPath, PrClass::ExecSurface]
}
