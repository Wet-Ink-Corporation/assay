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
//! [`OpenCommand`] reads `assay open`'s run from the Actions environment and refuses a re-run.
//! The pipeline's CI plumbing (#60, 03 §4a): [`RunEnv`] (the run, with the first-attempt guard
//! [`RunEnv::first_attempt`]), [`Needs`] (`ASSAY_NEEDS`, read strictly), [`Outputs`] (the
//! `GITHUB_OUTPUT` writer) and [`outputs`], each job's outputs as typed values. [`guard`] decides
//! whether the pipeline runs for an event and with which identity (`assay guard`, 0006).

mod ci;
mod guard;
mod open;
pub mod outputs;
mod prerequisites;
mod pull_request;
mod revisions;
mod status;
mod supervisor;

pub use ci::{
    EntryProblem, FirstAttempt, JobName, JobResult, MAX_NEEDS_BYTES, Needs, NeedsError,
    NotFirstAttempt, OutputError, OutputName, Outputs, RunEnv, RunEnvError, RunVariables, Upstream,
};
pub use guard::{GuardError, Guarded, Refusal, UntrustedHead, guard};
pub use open::{OpenCommand, OpenUsage, OpenVariables};
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
