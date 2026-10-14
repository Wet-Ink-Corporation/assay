//! assay application layer: the use cases (`open`, `plan`, `admit`, `gate`, `review`, `decide`,
//! `replay`, `report`, `doctor`) and the port traits they need.
//!
//! The ports (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`) are owned
//! here and implemented by the adapter crates (`docs/architecture/02-architecture.md` §2).
//! Every versioned contract is validated on read through [`ContractValidator`]
//! (`docs/architecture/03-contracts.md`). Base configuration is parsed into [`PluginsConfig`].

mod calendar;
mod check;
mod config;
mod contract;
mod coordinate;
mod decide;
mod doctor;
mod eval;
mod events;
mod gate;
mod golden;
mod model_runner;
mod plan;
mod preflight;
mod prompt_bundle;
mod publish;
mod replay;
mod rereview;
mod session_init;
mod shard;
mod specialists;
mod status;
mod tiering;

pub use calendar::{CalendarDate, CalendarDateError};
pub use doctor::{
    CanaryProbe, CanaryRun, CanaryToken, Check, CheckFailure, CheckResult, DoctorError,
    DoctorReport, EXPIRY_WARNING_DAYS, ExpiryWarning, doctor, token_expiry_warning,
};
pub use preflight::{
    AuthProof, CliProbe, CliStatus, FIRST_PARTY_PROVIDER, OAUTH_TOKEN_METHOD, PreflightFailed,
    PreflightFailure, PreflightPassed, StatusParts, StatusReadError, TOKEN_ROTATION_RUNBOOK,
    UNOBSERVED, is_provider_variable, preflight,
};
pub use session_init::{
    AllowlistDrift, InitParts, InitPlugin, InitReadError, InitUnclean, SessionInit,
    allowlist_drift, check_init,
};

pub use check::{
    ChangePatches, CheckError, CheckPorts, CheckRequest, FilePatch, GateErrorText, InvalidOutcome,
    LocalFinding, LocalGate, LocalOutcome, LocalReport, LocalReportError, LocalReview,
    MAX_DIFF_BYTES, MAX_FILE_LIST_BYTES, PatchBudget, ReviewSetup, ReviewStatus, check_local,
};
pub use config::{
    Adapter, Argv, AssaySettings, BuiltinAllowlist, BuiltinKind, BuiltinsError, CommitId,
    ConfigError, DEFAULT_TIMEOUT_SECS, ExactVersion, GateName, GlobsConfigError,
    InfraFailurePolicy, MAX_BUILTIN_NAME_BYTES, MAX_BUILTINS, MAX_TIMEOUT_SECS, ParseCommitIdError,
    ParseDigestError, ParsePinError, Pin, Plugin, PluginsConfig, PrClass, Promotion, RunnerKind,
    Sha256Digest, Step, TomlError, parse_globs,
};
pub use contract::{
    Contract, ContractSchemaError, ContractValidator, MAX_DOCUMENT_BYTES, ReadContractError,
    SchemaCompileError, SchemaViolation, parse_json_strict,
};
pub use coordinate::{
    COORDINATION_SCHEMA, COORDINATOR_CAP, COORDINATOR_MAX_TURNS, CoordinateRequest, Coordination,
    CoordinationRefusal, CoordinatorFailure, GroupId, MAX_COORDINATED_FINDINGS, Merged, MergedBy,
    Ranked, coordinate, merge_mechanically,
};
pub use decide::{
    AdmitJob, ArtifactDocument, AuthenticatedRun, Authentication, CurrentPullRequest, DecideError,
    DecideFailure, DecidePorts, DecideRequest, Fetched, GateJobOutputs, JobResult,
    MAX_EVIDENCE_BYTES, MAX_EVIDENCE_FILES, MAX_FETCHED_BYTES, MAX_REVIEW_DOCUMENTS,
    MAX_SYMBOL_BYTES, Moved, OutputsProblem, PlanJob, PlanMismatch, PlanRefusal, PullOnHead,
    PullRequestState, Refusal, Reported, ReportedGate, ReviewJob, RunArtifacts, Snapshot,
    StateStep, TierDegrade, Unreadable, UpstreamJob, authenticate,
};
pub use eval::{
    Acceptance, AuthoringModel, CASE_SCHEMA, CaseKind, CaseOutcome, CaseProblem, CaseSource,
    ClassMap, CompareError, CorpusError, EVAL_MONTHLY_CAP, EVAL_ROLE, EVAL_RUNS, EvalCase,
    EvalCorpus, EvalError, EvalLedger, EvalMonth, EvalReport, EvalRequest, LINE_WINDOW, Label,
    MAX_AUTHORING_MODEL_BYTES, MAX_CASE_BYTES, MAX_CASE_ID_BYTES, MAX_EVAL_CASES, MAX_EVAL_ENTRIES,
    MAX_LABEL_PATH_BYTES, MAX_LABELS, MAX_RUN_FINDINGS, NoiseCheck, OutputProblem, PathField,
    Proportion, RunScore, Split, Unscorable, WILSON_Z, compare, read_eval_corpus, run_eval,
};
pub use events::{EventSink, NoEvents};
pub use gate::{
    BuildJob, Conclusion, GateDocument, GateError, GateJob, GateOutcome, GateReport, GateRun,
    GateRunner, Termination, account_for_required, run_gates,
};
pub use golden::{CorpusEntry, CorpusEntryKind, GoldenReplay, SELF_GATE, replay_golden};
pub use model_runner::{
    InvalidPrompt, ModelAttempts, ModelCall, ModelOutcome, ModelResult, ModelRunner, Prompt,
    PromptError, RateLimitObserved, RetryError, Structured, run_with_retry,
};
pub use plan::{
    ArtifactJob, ArtifactNonce, BaseConfigError, ChangedFiles, EntryKind, EvidenceKey,
    FileListError, IncludeTargets, NonceSource, ObjectsStep, ParseNonceError, PlanError,
    PlanRequest, PlannedRun, PullRequestEvent, PullRequestObjects, REST_FILE_CAP, RestFile,
    RestFiles, TreeEntry, plan,
};
pub use prompt_bundle::{
    BundleFile, BundleFileError, BundleFiles, InvalidBundle, PromptBundle, Role,
};
pub use publish::{
    BUNDLE_FILE, BundleError, BundleStore, CheckConclusion, CheckName, CheckRun, CheckRunId,
    CheckState, CheckStatus, CheckUpdate, CommentForge, CommentId, CommentStep, Decided,
    EvidenceEntry, EvidencePointer, EvidenceRecord, FindingsError, FindingsOutcome,
    FindingsPublication, FindingsPublished, FindingsWritten, ForgeStep, InlineComment, LlmEvidence,
    MAX_BUNDLE_BYTES, MAX_INLINE_COMMENTS, MAX_PRIOR_COMMENTS, PersistFailure, Publication,
    PublishError, PublishFailure, PublishLedger, PublishOutcome, PublishPorts, Published,
    SUMMARY_MARKER, Stale, SummaryComment, Target, Unlock, Verdict, VerdictForge, Written, publish,
    publish_findings,
};
pub use replay::{
    BundleShape, BundleSource, DecisionDiff, Matched, Mismatch, ModelReplay, ReplayError,
    ReplayRequest, Replayed, RunField, StoredBundle, replay_decision,
};
pub use rereview::{
    Ancestry, CarriedAnchor, CarriedField, CarriedFinding, CarriedOutcome, CarriedParts,
    CarriedProblem, CarriedRefusal, CarriedState, Carry, CarryRefused, CarryReset, CarrySet,
    Cumulative, Increment, MAX_CARRIED, MAX_CARRIED_PATH_BYTES, MAX_RANGE_DIFF_BYTES,
    NotReverified, OmitReason, Omitted, PriorCarry, PriorCarryError, PriorHistory, RangeDiff,
    RangeDiffCommits, RangeDiffListing, ReviewTarget, TargetError, TargetRequest, next_carry,
    prior_carry, read_carried, recheck_anchors, review_target, settle_carried,
};
pub use shard::{
    Import, MAX_IMPORTS, MAX_SHARD_FILES, MAX_SHARDS, Shard, ShardCount, ShardError, ShardFile,
    Sharding, full_run_cap, shard,
};
pub use specialists::{
    HEAD_START, Halt, MAX_MODEL_PROCESSES, RoleFailure, RoleOutcome, RoleReport,
    ShardedSpecialists, ShardsError, ShardsRequest, Specialists, SpecialistsRequest, run_shards,
    run_specialists,
};
pub use status::{
    AppId, BundleProblem, CheckView, Detail, FetchedAt, Identities, InvalidFetchedAt,
    LocalStatusError, LocalStatusRequest, MAX_CHECK_RUNS, MAX_FINDINGS, MAX_REASONS,
    ObservedConclusion, PullSelector, PullView, ReviewCommentView, StatusError, StatusFailure,
    StatusForge, StatusPorts, StatusReport, StatusRepository, StatusRequest, StatusStep,
    StatusUnreadable, StoredLocalReport, UserId, Viewer, is_tree_id, local_status, status,
};
pub use tiering::{ExecSurface, MetadataError, PackageSet, Symlink, exec_surface};
pub use tiering::{baseline_change, exec_roots, manifest_exec_roots, manifest_trust_keys};

mod validation;
pub use validation::{
    Change, DiffPaths, EgressHit, EgressKind, EgressScan, EgressToken, EgressTokenError, FileDiff,
    FindingRef, FreeTextField, Hunk, Inputs, KnownRuns, KnownRunsError, LineSpan,
    MAX_INDEXED_BYTES, MAX_TOKEN_BYTES, Placement, ReviewerDocument, SuppressRules, Transition,
    ValidFinding, Validated, validate,
};

mod verify;
pub use verify::{
    ClaimRef, Judgement, MAX_REASON_CHARS, REJECT_RATE_MIN_RUNS, RejectRate, UnsupportedTier,
    VERDICT_SCHEMA, VERIFIER_CAP, VERIFIER_MAX_TURNS, VerdictReason, VerdictRefusal, VerdictTally,
    Verification, VerifiedFinding, VerifierFailure, VerifierStatus, VerifierTier, VerifyRequest,
    verify,
};
