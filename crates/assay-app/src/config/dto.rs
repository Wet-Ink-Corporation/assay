//! The wire shape of `.assay/plugins.toml`. Unknown keys are rejected at every level; the
//! values are validated when converted into the types of [`super`] (OUT-01).

use serde::Deserialize;

use super::assay::{
    InfraFailurePolicy, PrClass, Promotion, RunnerKind, default_block_on_infra_for,
};
use super::builtins::BuiltinsDto;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileDto {
    pub(super) assay: AssayDto,
    #[serde(default)]
    pub(super) gates: GatesDto,
    #[serde(default, rename = "plugin")]
    pub(super) plugins: Vec<PluginDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssayDto {
    pub(super) digest: String,
    pub(super) warm_target: Option<String>,
    pub(super) claude_cli: Option<String>,
    pub(super) claude_cli_builtins: Option<BuiltinsDto>,
    pub(super) runner: Option<RunnerKind>,
    #[serde(default)]
    pub(super) protected_bases: Vec<String>,
    #[serde(default)]
    pub(super) llm_blockers: Promotion,
    #[serde(default)]
    pub(super) llm_majors: Promotion,
    #[serde(default)]
    pub(super) on_infra_failure: InfraFailurePolicy,
    #[serde(default = "default_block_on_infra_for")]
    pub(super) block_on_infra_for: Vec<PrClass>,
    pub(super) ledger_repo: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GatesDto {
    #[serde(default)]
    pub(super) binary_globs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PluginDto {
    pub(super) name: String,
    pub(super) adapter: String,
    pub(super) required: bool,
    pub(super) pin: Option<String>,
    pub(super) timeout_s: Option<u64>,
    pub(super) exec: Option<Vec<String>>,
    pub(super) job: Option<String>,
    #[serde(rename = "step")]
    pub(super) steps: Option<Vec<StepDto>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StepDto {
    pub(super) name: String,
    pub(super) job: String,
    pub(super) cmd: Vec<String>,
    pub(super) timeout_s: u64,
}
