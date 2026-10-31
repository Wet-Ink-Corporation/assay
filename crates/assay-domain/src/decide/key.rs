//! The decision key of `docs/architecture/02-architecture.md` §5: the idempotency key of a
//! decision's publication, written as the verdict check's `external_id` (D14).
//!
//! It is encoded as the evidence key is (03 §4 `run_key`): SHA-256 over each field's name and
//! value in a fixed order, each prefixed by its byte length as a big-endian `u64`
//! ([`ContentHash::of_files`]), so no byte can move between two fields without changing it.

use std::fmt;

use crate::content_hash::ContentHash;
use crate::decide::inputs::InfraPolicy;

/// The fields of 02 §5's decision key, borrowed from what the application computed.
///
/// The digests and states are the application's: this crate only composes them. Every text
/// field is hashed exactly as given, so two callers agree on the key only when they agree on
/// these bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionKeyParts<'a> {
    /// The run's evidence key, `sha256:<hex>` (02 §3).
    pub evidence_key: &'a str,
    /// The digest of the head's approvals.
    pub approvals_digest: &'a str,
    /// The digest of the waivers the decision reads.
    pub waivers_digest: &'a str,
    /// The override the decision reads, or its absence.
    pub override_state: &'a str,
    /// The digest of the PR's valid dismissals.
    pub dismissals_digest: &'a str,
    /// Base policy's `on_infra_failure`.
    pub on_infra_failure: InfraPolicy,
    /// What the LLM stage's deferral rule reads.
    pub deferral_state: &'a str,
}

/// A decision key: displayed as `sha256:` and 64 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecisionKey(ContentHash);

impl fmt::Display for DecisionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The decision key of `parts`.
///
/// SHA-256 over the 02 §5 fields in order, each a length-prefixed name and value:
/// `evidence_key`, `approvals_digest`, `waivers_digest`, `override_state`, `dismissals_digest`,
/// `on_infra_failure_policy` (its wire code) and `deferral_state`.
///
/// # Examples
///
/// ```
/// use assay_domain::{DecisionKeyParts, InfraPolicy, decision_key};
///
/// let parts = DecisionKeyParts {
///     evidence_key: "sha256:00",
///     approvals_digest: "sha256:01",
///     waivers_digest: "sha256:02",
///     override_state: "null",
///     dismissals_digest: "sha256:03",
///     on_infra_failure: InfraPolicy::Block,
///     deferral_state: "{}",
/// };
/// let warn = DecisionKeyParts { on_infra_failure: InfraPolicy::Warn, ..parts };
///
/// assert!(decision_key(&parts).to_string().starts_with("sha256:"));
/// assert_ne!(decision_key(&parts), decision_key(&warn));
/// ```
#[must_use]
pub fn decision_key(parts: &DecisionKeyParts<'_>) -> DecisionKey {
    let fields: [(&str, &str); 7] = [
        ("evidence_key", parts.evidence_key),
        ("approvals_digest", parts.approvals_digest),
        ("waivers_digest", parts.waivers_digest),
        ("override_state", parts.override_state),
        ("dismissals_digest", parts.dismissals_digest),
        ("on_infra_failure_policy", parts.on_infra_failure.code()),
        ("deferral_state", parts.deferral_state),
    ];
    DecisionKey(ContentHash::of_files(
        fields
            .into_iter()
            .map(|(name, value)| (name, value.as_bytes())),
    ))
}
