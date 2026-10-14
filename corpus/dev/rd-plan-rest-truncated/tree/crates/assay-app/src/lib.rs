//! assay application layer: the use cases (`open`, `plan`, `admit`, `gate`, `review`, `decide`,
//! `replay`, `report`, `doctor`) and the port traits they need.
//!
//! The ports (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`) are owned
//! here and implemented by the adapter crates (`docs/architecture/02-architecture.md` §2).
//! Every versioned contract is validated on read through [`ContractValidator`]
//! (`docs/architecture/03-contracts.md`). Base configuration is parsed into [`PluginsConfig`].

mod config;
mod contract;
mod gate;
mod model_runner;
mod plan;
mod prompt_bundle;
mod tiering;

pub use config::{
    Adapter, Argv, AssaySettings, CommitId, ConfigError, DEFAULT_TIMEOUT_SECS, ExactVersion,
    GateName, GlobsConfigError, InfraFailurePolicy, MAX_TIMEOUT_SECS, ParseCommitIdError,
    ParseDigestError, ParsePinError, Pin, Plugin, PluginsConfig, PrClass, Promotion, RunnerKind,
    Sha256Digest, Step, TomlError, parse_globs,
};
pub use contract::{
    Contract, ContractSchemaError, ContractValidator, MAX_DOCUMENT_BYTES, ReadContractError,
    SchemaCompileError, SchemaViolation,
};
pub use gate::{
    BuildJob, Conclusion, GateDocument, GateError, GateJob, GateOutcome, GateReport, GateRun,
    GateRunner, Termination, account_for_required, run_gates,
};
pub use model_runner::{
    InvalidPrompt, ModelAttempts, ModelCall, ModelOutcome, ModelResult, ModelRunner, Prompt,
    PromptError, RateLimitObserved, RetryError, Structured, run_with_retry,
};
pub use plan::{
    ArtifactJob, ArtifactNonce, BaseConfigError, ChangedFiles, EntryKind, EvidenceKey,
    FileListError, IncludeTargets, NonceSource, ParseNonceError, PlanError, PlanRequest,
    PlannedRun, PullRequestEvent, PullRequestObjects, REST_FILE_CAP, RestFile, RestFiles,
    TreeEntry, plan,
};
pub use prompt_bundle::{
    BundleFile, BundleFileError, BundleFiles, InvalidBundle, PromptBundle, Role,
};
pub use tiering::{ExecSurface, MetadataError, PackageSet, Symlink, exec_surface};
pub use tiering::{baseline_change, exec_roots, manifest_exec_roots, manifest_trust_keys};

mod validation;
pub use validation::{
    Change, DiffPaths, FileDiff, FindingRef, Hunk, Inputs, LineSpan, Placement, SuppressRules,
    Transition, ValidFinding, Validated, validate,
};
