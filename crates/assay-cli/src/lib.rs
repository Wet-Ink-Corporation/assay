//! The `assay` composition root's adapters between crates that may not depend on each other
//! (ADR-07): what the binary lends one adapter from another.
//!
//! [`ForgeSupervisor`] lends `assay-forge`'s process-group supervisor to the gate adapters and
//! to `ClaudeHeadless` (wi-603c63), and runs the `gh api` and `gh run download` calls of
//! forge's GitHub transport (#273). [`ForgeRevisions`] lends forge's object-only repository to
//! the temper adapter as the PR's base and head files, and [`ForgePullRequest`] lends it to the
//! `plan` use case as the PR's objects. [`run_status`] wires `assay status` (#126) over any
//! [`assay_app::StatusForge`], so it is tested with fakes; the binary runs it over forge's
//! `GitHub` through `gh` (#273). [`run_prerequisites`] wires `assay doctor --repository` (#75)
//! over any [`assay_app::PrerequisiteForge`], tested with fakes; the binary does not run it
//! over the live transport yet.
//! [`OpenCommand`] reads `assay open`'s run from the Actions environment and refuses a re-run;
//! [`run_open`] opens the verdict check over any [`OpenReads`] and the app's open ports, so it
//! is tested with fakes; the binary runs it over forge's `GitHub` and ledger adapters.
//! The pipeline's CI plumbing (#60, 03 §4a): [`RunEnv`] (the run, with the first-attempt guard
//! [`RunEnv::first_attempt`]), [`Needs`] (`ASSAY_NEEDS`, read strictly), [`Outputs`] (the
//! `GITHUB_OUTPUT` writer) and [`outputs`], each job's outputs as typed values. [`guard`] decides
//! whether the pipeline runs for an event and with which identity (`assay guard`, 0006).
//! [`run_plan`] plans guard's PR over its fetched objects into [`RUN_FILE`] in an [`OutDir`]
//! and returns plan's outputs (`assay plan`, #60 ST-5). [`run_admit`] authenticates plan's
//! document, admits the run over the ledger, refs, bundle store, comments and fetched objects,
//! writes the admitted documents into an [`OutDir`] and returns admit's outputs (`assay admit`,
//! #60 ST-11).

mod admit;
mod ci;
mod guard;
mod open;
pub mod outputs;
mod plan;
mod prerequisites;
mod pull_request;
mod revisions;
mod status;
mod supervisor;

pub use admit::{
    AdmitJob, AdmitJobPorts, AdmitRunError, AdmitRunFailure, FetchCommits, FetchedSide, PACK_FILE,
    SuppressError, run_admit,
};
pub use ci::{
    EntryProblem, FirstAttempt, JobName, JobResult, MAX_NEEDS_BYTES, Needs, NeedsError,
    NotFirstAttempt, OutDir, OutDirError, OutputError, OutputName, Outputs, RunEnv, RunEnvError,
    RunVariables, Upstream,
};
pub use guard::{GuardError, Guarded, Refusal, UntrustedHead, guard};
pub use open::{
    OpenCommand, OpenJob, OpenReads, OpenRunError, OpenRunOutcome, OpenUsage, run_open,
};
pub use plan::{PlanJob, PlanPorts, PlanRunError, RUN_FILE, run_plan};
pub use prerequisites::{
    PrerequisitesArgs, PrerequisitesRunError, PrerequisitesUsage, prerequisites_exit_code,
    run_prerequisites, write_prerequisites,
};
pub use pull_request::{ForgeObjectsError, ForgePullRequest};
pub use revisions::ForgeRevisions;
pub use status::{
    Format, StatusArgs, StatusCommand, StatusContext, StatusRunError, StatusUsage,
    run as run_status,
};
pub use supervisor::ForgeSupervisor;
