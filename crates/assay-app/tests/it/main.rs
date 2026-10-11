//! Integration tests for `assay-app`, compiled as one binary (TST-14).

mod config;
mod contract;
mod gate;
mod redaction;
mod spec;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
