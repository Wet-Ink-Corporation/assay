//! The `assay` composition root's adapters between crates that may not depend on each other
//! (ADR-07): what the binary lends one adapter from another.
//!
//! [`ForgeSupervisor`] lends `assay-forge`'s process-group supervisor to the gate adapters and
//! to `ClaudeHeadless` (wi-603c63). [`ForgeRevisions`] lends forge's object-only repository to
//! the temper adapter as the PR's base and head files, and [`ForgePullRequest`] lends it to the
//! `plan` use case as the PR's objects. [`run_status`] wires `assay status` (#126) over any
//! [`assay_app::StatusForge`], so it is tested with fakes until the GitHub adapter (#53) exists.

mod pull_request;
mod revisions;
mod status;
mod supervisor;

pub use pull_request::{ForgeObjectsError, ForgePullRequest};
pub use revisions::ForgeRevisions;
pub use status::{
    Format, StatusArgs, StatusCommand, StatusContext, StatusRunError, StatusUsage,
    run as run_status,
};
pub use supervisor::ForgeSupervisor;
