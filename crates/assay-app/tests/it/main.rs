//! Integration tests for `assay-app`, compiled as one binary (TST-14).

mod config;
mod contract;
mod domain_model;
mod gate;
mod model_runner;
mod prompt_bundle;
mod redaction;
mod spec;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
