//! Integration tests for `assay-app`, compiled as one binary (TST-14).

mod builtins;
mod config;
mod contract;
mod decide;
mod decide_evidence;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod decide_fakes;
mod decide_refusals;
mod doctor;
mod domain_model;
mod exec_roots;
mod gate;
mod golden;
mod model_runner;
mod package_set;
mod plan;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod plan_fakes;
mod preflight;
mod prompt_bundle;
mod publish;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod publish_fakes;
mod publish_findings;
mod publish_persist;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod publish_support;
mod read_limits;
mod redaction;
mod self_trust;
mod session_init;
mod spec;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod tiering;
mod validation;
mod verify;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod verify_fakes;
mod verify_full;
