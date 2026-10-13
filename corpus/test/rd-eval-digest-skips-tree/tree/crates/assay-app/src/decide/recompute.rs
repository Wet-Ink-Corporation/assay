//! The plan, authenticated and recomputed (02 §4: `run.json` is not trusted by Z4): its
//! artifact must hash to the `plan` job's output, pass its schema, and equal the document Z4
//! writes from its own reading of base config and the PR's objects.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use assay_domain::Tier;
use serde_json::Value;

use super::fetch::{Artifact, Fetch};
use super::ports::RunArtifacts;
use super::refusal::{PlanMismatch, PlanRefusal, Refusal, Unreadable};
use super::request::{DecideRequest, JobResult, TierDegrade};
use super::{DecideError, Halt, Reader, RunDocument};
use crate::contract::Contract;
use crate::plan::{
    ArtifactJob, ArtifactNonce, PlanError, PullRequestObjects, Recomputed, recompute,
};

/// The file the `plan` artifact holds.
pub(super) const PLAN_FILE: &str = "run.json";

/// The plan as Z4 recomputed and authenticated it.
#[derive(Debug)]
pub(super) struct AuthenticPlan {
    pub(super) recomputed: Recomputed,
    pub(super) artifact_names: BTreeMap<ArtifactJob, String>,
    /// The `decide` job's own artifact, where the bundle goes.
    pub(super) bundle_artifact: String,
    /// The authentic `run.json`, for the bundle.
    pub(super) document: RunDocument,
    /// The tier the LLM stage was run at: the recomputed one, or `lite` after `admit`'s
    /// accepted degrade.
    pub(super) tier: Tier,
}

/// Authenticates the plan artifact and checks it against Z4's own recomputation.
pub(super) fn authenticate_plan<O: PullRequestObjects, S, A: RunArtifacts>(
    request: &DecideRequest,
    reader: &mut Reader<'_, O, A>,
) -> Result<AuthenticPlan, Halt<O::Error, S, A::Error>> {
    let job = &request.plan_job;
    let unreadable = |why| Refusal::ArtifactUnreadable {
        job: ArtifactJob::Plan,
        file: PLAN_FILE.to_owned(),
        why,
    };
    let artifact = Artifact {
        job: ArtifactJob::Plan,
        name: &job.artifact,
    };
    let bytes = match reader.fetcher.fetch(artifact, PLAN_FILE, &job.document)? {
        Fetch::Authentic(bytes) => bytes,
        Fetch::Missing => return Err(unreadable(Unreadable::Missing).into()),
        Fetch::TooLarge => return Err(unreadable(Unreadable::TooLarge).into()),
        Fetch::Tampered => {
            return Err(Refusal::ArtifactDigest {
                job: ArtifactJob::Plan,
                file: PLAN_FILE.to_owned(),
            }
            .into());
        }
    };
    let document = reader
        .validator
        .read(Contract::Run, &bytes)
        .map_err(|_invalid| Refusal::from(PlanMismatch::Invalid))?;
    let (artifact_names, bundle_artifact) =
        artifact_names(&document, &job.artifact).ok_or(PlanMismatch::ArtifactNames)?;
    let recomputed = recompute::<O, Infallible>(&request.plan, reader.objects).map_err(refused)?;
    let expected = recomputed
        .to_json(&request.plan, &artifact_names)
        .and_then(|json| serde_json::from_slice::<Value>(&json))
        .map_err(DecideError::Encode)?;
    if let Some(name) = first_difference(&expected, &document) {
        return Err(Refusal::from(PlanMismatch::Field { name }).into());
    }
    let tier = degraded_tier(request, &recomputed)?;
    let authentic = RunDocument {
        job: ArtifactJob::Plan,
        file: PLAN_FILE.to_owned(),
        // The bundle records the digest beside the bytes; the request keeps its own.
        digest: job.document.clone(),
        bytes,
    };
    Ok(AuthenticPlan {
        recomputed,
        artifact_names,
        bundle_artifact,
        document: authentic,
        tier,
    })
}

/// A recomputation error as a refusal when it is about the PR, as an error when the port
/// failed.
fn refused<O, S, A>(error: PlanError<O, Infallible>) -> Halt<O, S, A> {
    let refusal = match error {
        PlanError::DiffTooSlow => PlanRefusal::DiffTooSlow,
        PlanError::FileList(error) => PlanRefusal::FileList(error),
        PlanError::BaseConfig(error) => PlanRefusal::BaseConfig(error),
        PlanError::Nonce(never) => match never {},
        other => return Halt::Failed(DecideError::Recompute(other)),
    };
    Halt::Refused(Refusal::PlanMismatch(PlanMismatch::Recompute(refusal)))
}

/// The plan's artifact names, one `assay-<job>-<nonce>` per job with distinct nonces and its
/// own equal to the `plan` job's output, with the `decide` job's own; `None` otherwise.
fn artifact_names(
    document: &Value,
    plan_artifact: &str,
) -> Option<(BTreeMap<ArtifactJob, String>, String)> {
    let names = document.get("artifact_names")?.as_object()?;
    if names.len() != ArtifactJob::ALL.len() {
        return None;
    }
    let mut nonces = BTreeSet::new();
    let mut parsed = BTreeMap::new();
    for job in ArtifactJob::ALL {
        let name = names.get(job.name())?.as_str()?;
        let nonce: ArtifactNonce = name
            .strip_prefix("assay-")?
            .strip_prefix(job.name())?
            .strip_prefix('-')?
            .parse()
            .ok()?;
        if !nonces.insert(nonce) {
            return None;
        }
        parsed.insert(job, name.to_owned());
    }
    // The run keeps the bundle's artifact name beside the map, which owns its own.
    let bundle = parsed.get(&ArtifactJob::Decide)?.clone();
    let own = parsed.get(&ArtifactJob::Plan)? == plan_artifact;
    own.then_some((parsed, bundle))
}

/// The first top-level field, in key order, whose value differs between Z4's document and the
/// plan's, or that only one of them has.
fn first_difference(expected: &Value, actual: &Value) -> Option<String> {
    let (Some(expected), Some(actual)) = (expected.as_object(), actual.as_object()) else {
        // Not two objects: the whole document differs.
        return Some("document".to_owned());
    };
    let keys: BTreeSet<&String> = expected.keys().chain(actual.keys()).collect();
    keys.into_iter()
        .find(|key| expected.get(*key) != actual.get(*key))
        // Z4's own key when it has one; else the plan's, which the schema bounded.
        .map(|key| (*key).clone())
}

/// The tier the LLM stage ran at: `admit`'s full-to-lite degrade is accepted only from a
/// successful `admit`, on a recomputed `full` with no class (04 §2.4, R3-27); any other degrade
/// is a mismatch.
fn degraded_tier(request: &DecideRequest, recomputed: &Recomputed) -> Result<Tier, PlanMismatch> {
    let tiering = &recomputed.tiering;
    match request.admit.degrade {
        TierDegrade::None => Ok(tiering.tier),
        TierDegrade::FullToLite
            if request.admit.result == JobResult::Success
                && tiering.tier == Tier::Full
                && tiering.classes.is_empty() =>
        {
            Ok(Tier::Lite)
        }
        TierDegrade::FullToLite => Err(PlanMismatch::Degrade {
            tier: tiering.tier,
            // The refusal reports the classes it saw.
            classes: tiering.classes.clone(),
        }),
    }
}
