//! The `assay` composition root's adapters between crates that may not depend on each other
//! (ADR-07): what the binary lends one adapter from another.
//!
//! [`ForgeSupervisor`] lends `assay-forge`'s process-group supervisor to the gate adapters and
//! to `ClaudeHeadless` (wi-603c63), and runs the `gh api` and `gh run download` calls of
//! forge's GitHub transport (#273). [`ForgeRevisions`] lends forge's object-only repository to
//! the temper adapter as the PR's base and head files, and [`ForgePullRequest`] lends it to the
//! `plan` use case as the PR's objects. [`run_status`] wires `assay status` (#126) over any
//! [`assay_app::StatusForge`], so it is tested with fakes; the binary runs it over forge's
//! `GitHub` through `gh` (#273).
//! [`OpenCommand`] reads `assay open`'s run from the Actions environment and refuses a re-run.

mod open;
mod pull_request;
mod revisions;
mod status;
mod supervisor;

pub use open::{OpenCommand, OpenUsage, OpenVariables};
pub use pull_request::{ForgeObjectsError, ForgePullRequest};
pub use revisions::ForgeRevisions;
pub use status::{
    Format, StatusArgs, StatusCommand, StatusContext, StatusRunError, StatusUsage,
    run as run_status,
};
pub use supervisor::ForgeSupervisor;
