//! The `assay.run/1` document `plan` writes (`docs/architecture/03-contracts.md` §4), the
//! evidence key it carries as `run_key`, and the per-job artifact names.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use assay_domain::{
    ContentHash, FileChange, PrClass, RequiredRole, RunUid, Tier, TierReason, Tiering,
};
use serde::Serialize;

use super::PullRequestEvent;
use super::base_config::BaseConfig;
use crate::config::{Adapter, Plugin, Sha256Digest};

/// The contract and version `plan` writes.
const SCHEMA: &str = "assay.run/1";

/// The evidence key (`docs/architecture/02-architecture.md` §3), for evidence reuse only.
///
/// It identifies a run's expensive evidence, gates and model output. A requeue, a retry and
/// a same-head run share it, so it never keys a lease, a ledger record or the pending stamp;
/// `run_uid` does (R3-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EvidenceKey(ContentHash);

impl fmt::Display for EvidenceKey {
    /// `sha256:` and 64 lowercase hex digits.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<EvidenceKey> for Sha256Digest {
    /// The key as the digest it displays as, `sha256:<hex>`: how a job's outputs carry it.
    fn from(key: EvidenceKey) -> Self {
        Self::from(key.0)
    }
}

impl EvidenceKey {
    /// SHA-256 over the 02 §3 fields in order, each a length-prefixed name and value:
    /// `repo`, `pr`, `head_sha`, `base_sha`, `config_hash`, `assay_digest`,
    /// `claude_cli_version` (empty when unpinned), `prompt_bundle_hash` and `tool_pins` (one
    /// `<plugin>=<pin>` line per built-in gate, in file order).
    pub(super) fn new(
        pull_request: &PullRequestEvent,
        config: &BaseConfig,
        prompt_bundle: ContentHash,
    ) -> Self {
        let assay = config.plugins.assay();
        let pins: String = config
            .plugins
            .plugins()
            .iter()
            .filter_map(|plugin| pin(plugin).map(|pin| format!("{}={pin}\n", plugin.name())))
            .collect();
        let pr = pull_request.number.to_string();
        let config_hash = config.hash.to_string();
        let prompt_bundle = prompt_bundle.to_string();
        let fields: [(&str, &str); 9] = [
            ("repo", &pull_request.repo),
            ("pr", &pr),
            ("head_sha", pull_request.head_sha.as_str()),
            ("base_sha", pull_request.base_sha.as_str()),
            ("config_hash", &config_hash),
            ("assay_digest", assay.digest.as_str()),
            (
                "claude_cli_version",
                assay
                    .claude_cli
                    .as_ref()
                    .map_or("", |version| version.as_str()),
            ),
            ("prompt_bundle_hash", &prompt_bundle),
            ("tool_pins", &pins),
        ];
        Self(ContentHash::of_files(
            fields
                .into_iter()
                .map(|(name, value)| (name, value.as_bytes())),
        ))
    }
}

/// A job whose artifact another job of the run consumes, each named with its own nonce
/// (`docs/architecture/02-architecture.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArtifactJob {
    /// `plan`: this run description.
    Plan,
    /// `gates-static`: the Z2s gate documents.
    GatesStatic,
    /// `gates-lint`: the Z2b lint gate documents.
    GatesLint,
    /// `gates-test`: the Z2b test gate documents.
    GatesTest,
    /// `admit`: the context pack.
    Admit,
    /// `review`: the reviewers' findings.
    Review,
    /// `decide`: the persisted input bundle.
    Decide,
}

impl ArtifactJob {
    /// Every job, in pipeline order.
    pub const ALL: [Self; 7] = [
        Self::Plan,
        Self::GatesStatic,
        Self::GatesLint,
        Self::GatesTest,
        Self::Admit,
        Self::Review,
        Self::Decide,
    ];

    /// The job's name in the workflow, and its key in `artifact_names`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::GatesStatic => "gates-static",
            Self::GatesLint => "gates-lint",
            Self::GatesTest => "gates-test",
            Self::Admit => "admit",
            Self::Review => "review",
            Self::Decide => "decide",
        }
    }
}

/// The pin of a built-in gate; `None` for `exec`, which runs PR-controlled code and has none.
fn pin(plugin: &Plugin) -> Option<String> {
    match plugin.adapter() {
        Adapter::Temper { pin, .. } | Adapter::WeighIn { pin } => Some(pin.to_string()),
        Adapter::Exec { .. } => None,
    }
}

/// Everything the document is written from.
pub(super) struct Inputs<'a> {
    pub(super) pull_request: &'a PullRequestEvent,
    pub(super) run_uid: RunUid,
    pub(super) run_key: EvidenceKey,
    pub(super) config: &'a BaseConfig,
    pub(super) tiering: &'a Tiering,
    /// The changed files tiered.
    pub(super) files: &'a [FileChange],
    pub(super) artifact_names: &'a BTreeMap<ArtifactJob, String>,
}

/// `assay.run/1` as `plan` writes it: no `budget`, which `admit` adds.
#[derive(Debug, Serialize)]
struct RunDocument<'a> {
    schema: &'static str,
    run_key: String,
    run_uid: RunUid,
    repo: &'a str,
    pr: u64,
    base_sha: &'a str,
    head_sha: &'a str,
    base_ref: &'a str,
    tier: Tier,
    tier_reasons: &'a [TierReason],
    classes: &'a [PrClass],
    roles: &'a [RequiredRole],
    required_roles: &'a [RequiredRole],
    shards: Vec<Shard<'a>>,
    plugins: Vec<PluginEntry<'a>>,
    trust_paths_changed: &'a [String],
    exec_surface_changed: &'a [String],
    filtered_stats: FilteredStats<'a>,
    artifact_names: BTreeMap<&'static str, &'a str>,
    draft: bool,
}

#[derive(Debug, Serialize)]
struct Shard<'a> {
    files: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
struct PluginEntry<'a> {
    name: &'a str,
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job: Option<String>,
    steps: Vec<StepEntry<'a>>,
}

#[derive(Debug, Serialize)]
struct StepEntry<'a> {
    name: &'a str,
    job: String,
    timeout_s: u64,
}

#[derive(Debug, Serialize)]
struct FilteredStats<'a> {
    files: usize,
    lines: u64,
    excluded: &'a [String],
}

/// The document as JSON. A draft schedules no reviewer and no shard (04 §2.4: gates only), but
/// keeps the roles its tier requires, so `decide` knows what review it lacks; otherwise every
/// file left after filtering is one shard. `plan` holds neither the context pack nor the
/// import graph, so the split by dependency cluster is the review job's ([`crate::shard`]).
pub(super) fn to_json(inputs: &Inputs<'_>) -> Result<Vec<u8>, serde_json::Error> {
    let Inputs {
        pull_request,
        run_uid,
        run_key,
        config,
        tiering,
        files,
        artifact_names,
    } = inputs;
    let roles: &[RequiredRole] = if pull_request.draft {
        &[]
    } else {
        &tiering.required_roles
    };
    let reviewed = reviewed_files(files, &tiering.filtered.excluded);
    let shards = if roles.is_empty() || reviewed.is_empty() {
        Vec::new()
    } else {
        vec![Shard { files: reviewed }]
    };
    let document = RunDocument {
        schema: SCHEMA,
        run_key: run_key.to_string(),
        run_uid: *run_uid,
        repo: &pull_request.repo,
        pr: pull_request.number.get(),
        base_sha: pull_request.base_sha.as_str(),
        head_sha: pull_request.head_sha.as_str(),
        base_ref: &pull_request.base_ref,
        tier: tiering.tier,
        tier_reasons: &tiering.tier_reasons,
        classes: &tiering.classes,
        roles,
        required_roles: &tiering.required_roles,
        shards,
        plugins: config.plugins.plugins().iter().map(plugin_entry).collect(),
        trust_paths_changed: &tiering.trust_paths_changed,
        exec_surface_changed: &tiering.exec_surface_changed,
        filtered_stats: FilteredStats {
            files: tiering.filtered.files,
            lines: tiering.filtered.lines,
            excluded: &tiering.filtered.excluded,
        },
        artifact_names: artifact_names
            .iter()
            .map(|(job, name)| (job.name(), name.as_str()))
            .collect(),
        draft: pull_request.draft,
    };
    serde_json::to_vec_pretty(&document)
}

/// The files the reviewers read: every changed file filtering did not exclude, in change
/// order.
fn reviewed_files<'a>(files: &'a [FileChange], excluded: &[String]) -> Vec<&'a str> {
    let excluded: BTreeSet<&str> = excluded.iter().map(String::as_str).collect();
    files
        .iter()
        .map(|file| file.path.as_str())
        .filter(|path| !excluded.contains(path))
        .collect()
}

fn plugin_entry(plugin: &Plugin) -> PluginEntry<'_> {
    let (job, steps) = match plugin.adapter() {
        Adapter::Temper { steps, .. } => (
            None,
            steps
                .iter()
                .map(|step| StepEntry {
                    name: step.name().as_str(),
                    job: step.job().to_string(),
                    timeout_s: step.timeout().as_secs(),
                })
                .collect(),
        ),
        Adapter::WeighIn { .. } => (None, Vec::new()),
        Adapter::Exec { job, .. } => (Some(job.to_string()), Vec::new()),
    };
    PluginEntry {
        name: plugin.name().as_str(),
        required: plugin.required(),
        pin: pin(plugin),
        job,
        steps,
    }
}
