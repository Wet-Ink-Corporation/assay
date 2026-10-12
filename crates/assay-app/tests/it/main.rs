//! Integration tests for `assay-app`, compiled as one binary (TST-14).

mod config;
mod contract;
mod domain_model;
mod exec_roots;
mod gate;
mod golden;
mod model_runner;
mod package_set;
mod plan;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod plan_fakes;
mod prompt_bundle;
mod read_limits;
mod redaction;
mod self_trust;
mod spec;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod tiering;
mod validation;
