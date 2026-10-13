//! assay runner adapter: builds and supervises `claude -p` processes, parses their
//! `stream-json` output and accounts for cost.
//!
//! Implements the `ModelRunner` port of `assay-app` (`docs/architecture/03-contracts.md` §7):
//! [`ClaudeHeadless`] runs `claude -p` under the headless profile of 03 §8, and [`Replay`]
//! answers from recorded transcripts. Both classify a transcript the same way.

mod cost;
mod headless;
mod replay;
mod status;
mod stream;

pub use headless::{
    CanaryError, ClaudeCanary, ClaudeHeadless, EnvVarError, GroupKill, HeadlessEnv, HeadlessError,
    StatusCommand, StatusError, Workdir, WorkdirError,
};
pub use replay::{Recording, Replay, ReplayError};
pub use status::read_cli_status;
pub use stream::read_session_init;
