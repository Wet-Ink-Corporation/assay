//! Integration tests for `assay-runner`, compiled as one binary (TST-14). No test starts the real
//! `claude`: `Replay` reads recorded streams, and `ClaudeHeadless` runs a stand-in script.

mod canary;
mod headless;
mod init;
mod replay;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
