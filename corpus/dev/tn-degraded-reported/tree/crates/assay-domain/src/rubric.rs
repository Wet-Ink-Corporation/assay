//! The decision rubric: the data `decide` reads (`rubric/v1.toml`,
//! `docs/architecture/04-pipeline.md` §5).
//!
//! The domain owns the rubric's meaning but reads no files: the caller parses the TOML into
//! [`Rubric`], which validates on deserialisation.

use std::num::NonZeroU32;

use serde::Deserialize;

use crate::decide::digest::{Canonical, Encoder};

/// A validated decision rubric (04 §5): the thresholds and lists the rules read.
///
/// Built only by deserialising (for example `rubric/v1.toml`); an empty version is rejected,
/// because every decision records it as `rubric_version`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "RubricFile")]
pub struct Rubric {
    version: String,
    majors_block_threshold: NonZeroU32,
    max_requeues: u32,
    micro_roles: Vec<String>,
    waivable: Vec<WaivableRule>,
}

/// The wire form of a rubric file, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RubricFile {
    version: String,
    majors_block_threshold: NonZeroU32,
    max_requeues: u32,
    micro_roles: Vec<String>,
    #[serde(default)]
    waivable: Vec<WaivableRule>,
}

/// One kind of human-decision gate finding an owner may waive: a rule of one gate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaivableRule {
    gate: String,
    rule: String,
}

/// Why a rubric was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RubricError {
    /// The `version` is empty, so decisions could not record which rubric made them.
    #[error("the rubric version is empty")]
    EmptyVersion,
}

impl TryFrom<RubricFile> for Rubric {
    type Error = RubricError;

    fn try_from(file: RubricFile) -> Result<Self, Self::Error> {
        let RubricFile {
            version,
            majors_block_threshold,
            max_requeues,
            micro_roles,
            waivable,
        } = file;
        if version.is_empty() {
            return Err(RubricError::EmptyVersion);
        }
        Ok(Self {
            version,
            majors_block_threshold,
            max_requeues,
            micro_roles,
            waivable,
        })
    }
}

impl Canonical for Rubric {
    fn encode(&self, encoder: &mut Encoder) {
        let Self {
            version,
            majors_block_threshold,
            max_requeues,
            micro_roles,
            waivable,
        } = self;
        version.encode(encoder);
        majors_block_threshold.get().encode(encoder);
        max_requeues.encode(encoder);
        micro_roles.encode(encoder);
        waivable.encode(encoder);
    }
}

impl Canonical for WaivableRule {
    fn encode(&self, encoder: &mut Encoder) {
        let Self { gate, rule } = self;
        gate.encode(encoder);
        rule.encode(encoder);
    }
}

impl Rubric {
    /// The version every decision records as `rubric_version`.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Rule 7: how many distinct root causes of confirmed majors fail the PR (`A-07`).
    #[must_use]
    pub const fn majors_block_threshold(&self) -> NonZeroU32 {
        self.majors_block_threshold
    }

    /// Rule 8a: how many times a deferred run is requeued before it fails
    /// `infra-llm-unavailable`.
    #[must_use]
    pub const fn max_requeues(&self) -> u32 {
        self.max_requeues
    }

    /// Whether findings from `role` are advisory and never count for rule 7 (the micro tier).
    #[must_use]
    pub fn is_micro_role(&self, role: &str) -> bool {
        self.micro_roles.iter().any(|micro| micro == role)
    }

    /// Whether a finding of `rule` from `gate` is a human-decision finding an owner may waive.
    #[must_use]
    pub fn is_waivable(&self, gate: &str, rule: &str) -> bool {
        self.waivable
            .iter()
            .any(|waivable| waivable.gate == gate && waivable.rule == rule)
    }
}
