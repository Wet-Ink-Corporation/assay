//! Integration tests for `assay-runner`, compiled as one binary (TST-14). No test starts the real
//! `claude`: `Replay` reads recorded streams, and `ClaudeHeadless` runs a stand-in script.

mod canary;
mod coordinate;
mod headless;
mod init;
mod profile_canary;
mod replay;
mod status;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod verify;
