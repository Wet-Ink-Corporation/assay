//! Integration tests for `assay-app`, compiled as one binary (TST-14).

mod config;
mod contract;
mod domain_model;
mod exec_roots;
mod gate;
mod model_runner;
mod prompt_bundle;
mod read_limits;
mod redaction;
mod spec;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod tiering;
