//! `plan`'s outputs (#60 ST-5, 03 §4a): the digest of the `run.json` it wrote, the run's
//! evidence key, tier and draft flag, whether it gets an LLM review, git's merge base of the
//! pinned commits (D4 A), and each job's artifact name.

use std::collections::BTreeMap;

use assay_app::{ArtifactJob, ArtifactNonce, CommitId, PlannedRun, Sha256Digest, reviewed};
use assay_domain::Tier;

use super::{Read, flag_word, parse_flag};
use crate::ci::{JobName, Needs, NeedsError, OutputName};

/// The job whose outputs [`PlanOutputs`] are.
const PLAN: JobName = JobName::Plan;

/// What the `plan` job reports of the run it planned.
///
/// Built only from a planned run ([`PlanOutputs::new`]) or from plan's own outputs
/// ([`PlanOutputs::from_needs`]), so it always names all seven artifacts, and its `review`
/// is derived from the tier and the draft flag, never stored beside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanOutputs {
    run_digest: Sha256Digest,
    run_key: Sha256Digest,
    tier: Tier,
    draft: bool,
    merge_base: CommitId,
    artifacts: BTreeMap<ArtifactJob, String>,
}

impl PlanOutputs {
    /// The outputs of `planned`, whose document's bytes digest to `run_digest`, for a PR that
    /// is (`draft`) or is not a draft, with `merge_base` of its pinned commits.
    #[must_use]
    pub fn new(
        planned: &PlannedRun,
        run_digest: Sha256Digest,
        draft: bool,
        merge_base: CommitId,
    ) -> Self {
        Self {
            run_digest,
            run_key: planned.run_key().into(),
            tier: planned.tiering().tier,
            draft,
            merge_base,
            // The outputs own a copy: the planned run keeps its names for its document.
            artifacts: planned.artifact_names().clone(),
        }
    }

    /// The sha256 of the exact bytes of the `run.json` plan wrote.
    #[must_use]
    pub const fn run_digest(&self) -> &Sha256Digest {
        &self.run_digest
    }

    /// The run's evidence key, `sha256:<hex>`, as the document's `run_key`.
    #[must_use]
    pub const fn run_key(&self) -> &Sha256Digest {
        &self.run_key
    }

    /// The run's tier.
    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.tier
    }

    /// Whether the PR is a draft.
    #[must_use]
    pub const fn draft(&self) -> bool {
        self.draft
    }

    /// Whether the run gets an LLM review ([`assay_app::reviewed`]).
    #[must_use]
    pub const fn review(&self) -> bool {
        reviewed(self.tier, self.draft)
    }

    /// git's merge base of the pinned base and head.
    #[must_use]
    pub const fn merge_base(&self) -> &CommitId {
        &self.merge_base
    }

    /// Each job's artifact name, `assay-<job>-<nonce>`.
    #[must_use]
    pub const fn artifacts(&self) -> &BTreeMap<ArtifactJob, String> {
        &self.artifacts
    }

    /// plan's outputs, in [`OutputName::PLAN`]'s order.
    #[must_use]
    pub fn outputs(&self) -> Vec<(OutputName, String)> {
        let facts = [
            (OutputName::RunDigest, self.run_digest.to_string()),
            (OutputName::RunKey, self.run_key.to_string()),
            (OutputName::Tier, self.tier.code().to_owned()),
            (OutputName::Draft, flag_word(self.draft).to_owned()),
            (OutputName::Review, flag_word(self.review()).to_owned()),
            (OutputName::MergeBase, self.merge_base.to_string()),
        ];
        let artifacts = self
            .artifacts
            .iter()
            .map(|(&job, name)| (OutputName::artifact(job), name.clone()));
        facts.into_iter().chain(artifacts).collect()
    }

    /// The outputs plan wrote, read from `needs`. An empty output reads as not written.
    ///
    /// # Errors
    ///
    /// [`NeedsError::MissingJob`] without `plan`; [`NeedsError::NotSucceeded`] when it did
    /// not succeed; [`NeedsError::UnexpectedOutput`] for an output that is not plan's;
    /// [`NeedsError::MissingOutput`] for one absent or empty (every one is required);
    /// [`NeedsError::Output`] for one that does not parse, an artifact name that is not
    /// `assay-<job>-<32 lowercase hex>` for its own job, or a `review` other than the one its
    /// tier and draft flag give.
    pub fn from_needs(needs: &Needs) -> Result<Self, NeedsError> {
        let read = Read::succeeded(needs, PLAN, &OutputName::PLAN)?;
        let mut artifacts = BTreeMap::new();
        for job in ArtifactJob::ALL {
            let name = read.parsed(OutputName::artifact(job), |text| artifact(job, text))?;
            artifacts.insert(job, name);
        }
        let outputs = Self {
            run_digest: read.parsed(OutputName::RunDigest, |text| text.parse().ok())?,
            run_key: read.parsed(OutputName::RunKey, |text| text.parse().ok())?,
            tier: read.parsed(OutputName::Tier, |text| {
                Tier::ALL.into_iter().find(|tier| tier.code() == text)
            })?,
            draft: read.parsed(OutputName::Draft, parse_flag)?,
            merge_base: read.parsed(OutputName::MergeBase, |text| text.parse().ok())?,
            artifacts,
        };
        if read.parsed(OutputName::Review, parse_flag)? == outputs.review() {
            Ok(outputs)
        } else {
            Err(read.invalid(OutputName::Review))
        }
    }
}

/// `text` as `job`'s artifact name, `assay-<job>-<nonce>`.
fn artifact(job: ArtifactJob, text: &str) -> Option<String> {
    let nonce = text
        .strip_prefix("assay-")?
        .strip_prefix(job.name())?
        .strip_prefix('-')?;
    nonce
        .parse::<ArtifactNonce>()
        .ok()
        .map(|_nonce| text.to_owned())
}

#[cfg(test)]
mod tests;
