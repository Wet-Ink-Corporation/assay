//! assay application layer: the use cases (`open`, `plan`, `admit`, `gate`, `review`, `decide`,
//! `replay`, `report`, `doctor`) and the port traits they need.
//!
//! The ports (`ModelRunner`, `Forge`, `EventSink`, `GateRunner`, `Ledger`, `Clock`) are owned
//! here and implemented by the adapter crates (`docs/architecture/02-architecture.md` §2).
//! Every versioned contract is validated on read through [`ContractValidator`]
//! (`docs/architecture/03-contracts.md`). Base configuration is parsed into [`PluginsConfig`].

mod admit;
mod calendar;
mod check;
mod clock;
mod config;
mod contract;
mod coordinate;
mod decide;
mod doctor;
mod eligibility;
mod eval;
mod events;
mod finding_text;
mod forge;
mod gate;
mod golden;
mod ledger;
mod model_runner;
mod open;
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

pub use admit::{
    ADMISSIONS_PER_HOUR, Admission, AdmitError, AdmitFailure, AdmitPorts, AdmitRequest, Admitted,
    AfterSlot, Budget, ContextPack, DAY_CAP, Deferred, MAX_DISMISSALS, MAX_SUPPRESS_BYTES,
    PlanDocument, PlanStage, RangeIdentities, SLOT_POLL, SLOT_POLLS, Spend, Unwound, admit,
    plan_stage,
};
pub use calendar::{CalendarDate, CalendarDateError};
pub use clock::Clock;
pub use doctor::{
    AGENTS_PATH, Access, AppliedRule, BaseError, BranchPolicy, BypassActor, CanaryProbe, CanaryRun,
    CanaryToken, Check, CheckFailure, CheckResult, Collaborator, Conditions, DeploymentBranches,
    DirectoryEntry, DoctorError, DoctorReport, EXPIRY_WARNING_DAYS, Enforcement, EntryType,
    ExpiryWarning, Holding, IdentityLists, OWNERS_PATH, PLUGINS_PATH, PolicyKind, Prerequisite,
    PrerequisiteApps, PrerequisiteBase, PrerequisiteFailure, PrerequisiteForge, PrerequisiteReport,
    PrerequisiteResult, RequiredCheck, RequiredChecks, Rule, Ruleset, RulesetId, RulesetTarget,
    WorkflowError, WorkflowPermissions, YamlError, YamlRefusal, doctor, prerequisites, read_base,
    token_expiry_warning,
};
pub use doctor::{
    FENCE_TARGET, FlagResult, HeadlessFlag, MIN_CLI_VERSION, PROFILE_SCHEMA, PermissionDenial,
    ProfileCheck, ProfileContext, ProfileError, ProfileFailure, ProfilePorts, ProfileProbe,
    ProfileReport, ProfileResult, ProfileRun, ProfileVerdict, ProfileWords, ReplyField,
    verify_profile,
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
    ConfigError, DEFAULT_TIMEOUT_SECS, EligibilityLists, ExactVersion, GateName, GlobsConfigError,
    Identity, ListFile, ListName, ListsError, MAX_BUILTIN_NAME_BYTES, MAX_BUILTINS,
    MAX_LIST_ENTRIES, MAX_LISTS_BYTES, MAX_TIMEOUT_SECS, ParseCommitIdError, ParseDigestError,
    ParseGateNameError, ParsePinError, Pin, Plugin, PluginsConfig, RunnerKind, Sha256Digest, Step,
    TomlError, parse_globs,
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
    AdmitJob, AdmitOutcome, ArtifactDocument, AuthenticatedRun, Authentication, CurrentPullRequest,
    DecideError, DecideFailure, DecidePorts, DecideRequest, Fetched, GateJobOutputs, InputsError,
    InputsRequest, JobResult, MAX_EVIDENCE_BYTES, MAX_EVIDENCE_FILES, MAX_FETCHED_BYTES,
    MAX_REVIEW_DOCUMENTS, MAX_SYMBOL_BYTES, Moved, OutputsProblem, PlanJob, PlanMismatch,
    PlanRefusal, PullOnHead, PullRequestState, Refusal, Reported, ReportedGate, ReviewJob,
    ReviewStage, RunArtifacts, Snapshot, StateStep, TierDegrade, UnknownReviewStage, Unreadable,
    UpstreamJob, authenticate, decision_inputs,
};
pub use eligibility::{
    AckedSet, Command, CommandComments, CommandCount, CommandRejected, CommandTimeError,
    CommentAction, CommentEvent, CommitRange, Counted, Exempt, GitHubState, IgnoredApproval,
    Ineligible, InvalidTimestamp, IssueComment, LedgerRecords, LedgerSnapshot, LedgerSnapshotError,
    MAX_COMMAND_REASON_CHARS, MAX_COMMAND_RECORDS, MAX_COMMANDS_PER_ACTOR, MAX_COMMENT_BYTES,
    MAX_PUSH_RECORDS, MAX_RANGE_COMMITS, MergeTree, ParseTreeIdError, Parsed, PriorWaivers,
    RangeCommit, RangeError, RangeListing, RangeObjects, RangeRequest, Recorded, RecordedState,
    RedecidePorts, RedecideRequest, RedecideStateError, SnapshotCommand, SnapshotLedger,
    SnapshotStep, StateError, StatePorts, StateRequest, Timestamp, TreeId, UnreadCommand,
    VoidCause, WEB_FLOW, WaiverBindings, count_command, github_state, parse_command,
    range_identities, redecided_state, waiver_events,
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
pub use finding_text::{clip_chars, plain_name};
pub use forge::{
    Actor, CheckFound, CommitIdentity, CompareAndSetError, Deadline, Forge, LockedWrite, Reaction,
    ReactionContent, ReactionSubject, Review, ReviewCommentId, ReviewState, TimelineEvent,
    TimelineKind, Upserted,
};
pub use gate::{
    BuildJob, GateDocument, GateError, GateJob, GateJobOutcome, GateJobReport, GateRun, GateRunner,
    ParseGateErrorError, Termination, account_for_required, run_gates,
};
pub use golden::{
    CorpusEntry, CorpusEntryKind, GoldenReplay, RubricError, SELF_GATE, read_rubric, replay_golden,
};
pub use ledger::{
    ActorCounter, Appended, BUDGET_WINDOW, COOLDOWN_WINDOW, CommandRecord, Consumed, Corruption,
    CostKind, CostRecord, CostRole, CostWindow, DecisionPointer, DeferralRecord, FileVersion,
    GrantKey, Granted, LOCK_TTL, LeaseError, LeaseRef, LeaseReservation, LeaseState, Ledger,
    LedgerFile, LedgerMonth, LedgerPath, LedgerReadError, LedgerRefs, LedgerTime, LedgerWriteError,
    LockAcquired, LockHold, LockLease, LockRequest, MAX_CAS_ATTEMPTS, MAX_COMMAND_CHARS,
    MAX_LEASE_MESSAGE_BYTES, MAX_LEDGER_FILE_BYTES, MAX_RECORD_BYTES, MAX_REVIEW_TIMEOUT,
    MAX_STATE_ENTRIES, ParseLedgerTimeError, PrState, PushAppended, PushDraft, PushKind,
    PushRecord, RefCreation, RefDeletion, RefTip, Released, RetryGrant, RoleCost, SLOT_TTL, Slot,
    SlotAcquired, SlotAcquisition, SlotHold, SlotLease, SlotRequest, SlotView, StateUpdate,
    WriteCondition, WriteOutcome, acquire_lock, acquire_slot, append_command, append_cost,
    append_push, consume_grant, grant_retry, parse_aeu, pending_grant, read_commands, read_costs,
    read_pushes, read_slots, read_state, reap_expired_slots, release_lock, release_slot,
    update_state,
};
pub use model_runner::{
    InvalidPrompt, ModelAttempts, ModelCall, ModelOutcome, ModelResult, ModelRunner, Prompt,
    PromptError, RateLimitObserved, RetryError, Structured, run_with_retry,
};
pub use open::{
    MAX_LOCK_RETRIES, OpenClock, OpenError, OpenFailure, OpenOutcome, OpenPorts, OpenRequest,
    OpenStep, OpenTrigger, Opened, PendingStamp, PushEvent, ReopenRefused, Stamped, open,
};
pub use plan::{
    ArtifactJob, ArtifactNonce, BaseConfigError, ChangedFiles, EntryKind, EvidenceKey,
    FileListError, IncludeTargets, NonceSource, ObjectsStep, ParseNonceError, PlanError,
    PlanRequest, PlannedRun, PullRequestEvent, PullRequestObjects, REST_FILE_CAP, RestFile,
    RestFiles, TreeEntry, plan, reviewed,
};
pub use prompt_bundle::{
    BundleFile, BundleFileError, BundleFiles, InvalidBundle, PromptBundle, Role,
};
pub use publish::{
    BUNDLE_FILE, BundleError, BundleStore, CheckConclusion, CheckName, CheckRun, CheckRunId,
    CheckState, CheckStatus, CheckUpdate, CommentForge, CommentId, CommentStep, Decided,
    EvidenceEntry, EvidencePointer, EvidenceRecord, FindingsError, FindingsOutcome,
    FindingsPublication, FindingsPublished, FindingsWritten, ForgeStep, InlineComment,
    LedgerPublish, LedgerPublishError, LlmEvidence, MAX_BUNDLE_BYTES, MAX_INLINE_COMMENTS,
    MAX_PRIOR_COMMENTS, PersistFailure, Publication, PublishError, PublishFailure, PublishLedger,
    PublishOutcome, PublishPorts, Published, SUMMARY_MARKER, Stale, SummaryComment, Target, Unlock,
    Verdict, VerdictForge, Written, publish, publish_findings,
};
pub use publish::{
    BundleRefused, PersistOutcome, Persisted, PersistedError, PersistedOutcome, PersistedPorts,
    PrepareError, PreparedBundle, Unconfirmed, UnknownPersistOutcome, prepare, publish_persisted,
};
pub use publish::{MAX_REFUSAL_DETAIL_BYTES, NotARefusal, RefusalReason, publish_refusal};
pub use replay::{
    BundleShape, BundleSource, DecisionBundleSource, DecisionDiff, Matched, Mismatch, ModelReplay,
    ReplayError, ReplayRequest, Replayed, RunField, StoredBundle, replay_decision,
};
pub use rereview::{
    Ancestry, CarriedAnchor, CarriedField, CarriedFinding, CarriedOutcome, CarriedParts,
    CarriedProblem, CarriedRefusal, CarriedState, Carry, CarryRefused, CarryReset, CarrySet,
    Cumulative, Increment, MAX_CARRIED, MAX_CARRIED_PATH_BYTES, MAX_RANGE_DIFF_BYTES,
    NotReverified, OmitReason, Omitted, PriorCarry, PriorCarryError, PriorHistory, RangeDiff,
    RangeDiffCommits, RangeDiffListing, ReviewTarget, TargetError, TargetRequest, next_carry,
    prior_carry, prior_head, prior_waivers, read_carried, recheck_anchors, review_target,
    settle_carried,
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
    AppId, BaseIdentitiesError, BundleProblem, CheckView, Detail, FetchedAt, Identities,
    InvalidFetchedAt, LocalStatusError, LocalStatusRequest, MAX_CHECK_RUNS, MAX_FINDINGS,
    MAX_REASONS, ObservedConclusion, PullSelector, PullView, ReviewCommentView, StatusError,
    StatusFailure, StatusForge, StatusPorts, StatusReport, StatusRepository, StatusRequest,
    StatusStep, StatusUnreadable, StoredLocalReport, UserId, Viewer, base_identities, base_lists,
    is_tree_id, local_status, read_pull, status,
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

mod reconcile;
pub use reconcile::{
    ChecksReconcileError, ChecksReconcileRequest, ChecksReconciled, ClosedCheck, Closure,
    LLM_PENDING_LABEL, LedgerReconcileError, LedgerReconcileRequest, LedgerReconciled, Listing,
    LockListing, LostLock, MAX_HEAD_CHECKS, MAX_OPEN_PULLS, MAX_PUBLISH_LOCKS,
    MAX_SUPERSEDED_HEADS, OpenPull, PullHead, ReconcileCheck, ReconcileForge, ReconcilePorts,
    RunProgress, STUCK_AFTER, Skip, SkippedCheck, StampedRun, reconcile_checks, reconcile_ledger,
};
mod release;
pub use release::{
    ReleaseError, ReleaseFailure, ReleasePorts, ReleaseRequest, ReviewCost, Settled, release,
};
mod weigh_in;
pub use weigh_in::{
    BlockingItem, Checkpoint, MAX_OPEN_BYTES, MAX_OPEN_ITEMS, OpenItemsError, checkpoint,
};
mod decide_only;
pub use decide_only::{
    CommandReplies, CompletedVerdict, MAX_POLL_PULLS, MAX_POLL_SELECTED, MAX_TIP_LIST_BYTES,
    PollError, PollForge, PolledPull, Reply, TipListFiles, TipLists, VerdictView, poll_pending,
};
mod provenance;
pub use provenance::{
    ExpectedRun, ProvenanceRefusal, RunEvent, RunProvenance, Workflow, WorkflowRunFacts, accept_run,
};
mod redecide;
pub use redecide::{
    Absence, DECISION_FILE, DecisionBundle, DecisionBundleError, DecisionReadBack, DecisionTarget,
    DecisionTrigger, EvidenceBundle, EvidenceError, EvidenceRead, EvidenceRequest, Forgery,
    MAX_DECISION_BUNDLE_BYTES, PrepareDecisionError, Redecided, RedecisionRequest, TriggerCommand,
    prepare_decision, read_decision_bundle, read_evidence, redecision_inputs,
};
